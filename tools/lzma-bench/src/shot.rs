//! `--shot`: one measurement, one process, one line of JSON.
//!
//! The other lanes time this crate and its oracles from inside one process,
//! which is the right shape for a ratio and the wrong one for memory: every
//! contender's peak resident set lands in the same process. A shot runs
//! exactly one contender once and exits, so a harness that launches it as a
//! child reads the contender's own peak from the kernel's accounting of that
//! child, the same way it reads `7zz`'s or `xz`'s. `bench/lzma-turbo-bench`
//! is that harness.
//!
//! Every shot streams its input from the file, as `7zz` and `xz` do, so the
//! input is not resident unless the API being timed needs all of it; the
//! line says when it did (`input_buffered_bytes`). The line carries what the
//! harness checks and reports: bytes in and out, the time spent in the timed
//! region, and the high-water mark of bytes this process had allocated while
//! it ran.
//!
//! A decoder only counts its output, as `xz -dc > /dev/null` does, unless
//! asked with `--verify` for a CRC-32 of the decoded bytes, which the harness
//! compares with the source file's. That costs a pass over the output only
//! this side would make, so the harness verifies in a run of its own and
//! times the others without it. The filter lane hashes outside its timed
//! region and always reports the CRC.
//!
//! ```text
//! lzma-bench --shot info
//! lzma-bench --shot <lane> [--threads N] [--preset P] [--mf-threads N]
//!            [--filter F] [--direction encode|decode] [--memory-limit B]
//!            [--verify] <file>
//! ```

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

use lzma_turbo::xz::{CheckType, FilterFlags, XzOptions, XzParallelReader, XzReader};
use lzma_turbo::{
    DrainStatus, Lzma2AdaptiveDecoder, Lzma2MtOptions, Lzma2ParallelDecoder, Lzma2Reader,
    LzmaEncProps, LzmaReader, XzWriter,
};

use crate::{Crc32, OUT_CHUNK, alloc_watch_peak, alloc_watch_reset, dict_size_from_prop};

const IN_CHUNK: usize = 1 << 20;

pub const HELP: &str = "\
  --shot LANE    run one measurement and print one JSON line, for a harness
                 that measures each contender as its own process:
                   info           build facts: version, decode loop, CRC tier
                   lzma1          LzmaReader over a .lzma file
                   lzma2          Lzma2Reader over the LZMA2 stream of a
                                  single-block .xz or a single-folder .7z
                   lzma2-mt       Lzma2ParallelDecoder, --threads N
                   adaptive       Lzma2AdaptiveDecoder fed the stream in
                                  4 MiB pieces it takes ownership of,
                                  --threads N, optional --memory-limit B
                   rust2-mt       lzma-rust2 Lzma2ReaderMt, --threads N
                   xz             XzReader over a whole .xz file
                   xz-par         XzParallelReader, --threads N
                   liblzma        the liblzma crate (C), --threads N
                   encode         XzWriter at --preset P, --threads N,
                                  optional --filter F
                   filter         one converter over the whole file in memory,
                                  --filter F --direction encode|decode
                 F is x86, arm, armthumb, arm64, ppc, sparc, ia64, riscv,
                 delta:N (distance N) or bcj2 (filter lane only)
  --memory-limit B  the adaptive lane's memory limit in bytes, or with an
                 M or G suffix in MiB or GiB (default: none)
  --verify       a decoder shot also reports a CRC-32 of what it decoded,
                 at the cost of a pass over it inside the timed region";

/// What a shot was asked to do.
pub struct Shot {
    pub lane: String,
    pub threads: usize,
    pub preset: u32,
    pub mf_threads: u32,
    pub filter: Option<String>,
    pub encode: bool,
    /// Whether a decoder hashes its output for the harness to check.
    pub verify: bool,
    /// The adaptive lane's memory limit.
    pub memory_limit: u64,
}

/// What a shot reports.
#[derive(Default)]
struct Line {
    bytes_in: u64,
    bytes_out: u64,
    /// The output's CRC-32, where the shot computed one.
    crc32: Option<u32>,
    seconds: f64,
    peak_alloc: u64,
    input_buffered: u64,
}

pub fn run(shot: &Shot, file: Option<&Path>) -> i32 {
    if shot.lane == "info" {
        println!("{}", info());
        return 0;
    }
    let Some(path) = file else {
        eprintln!("lzma-bench: --shot {} needs an input file", shot.lane);
        return 2;
    };
    match measure(shot, path) {
        Ok(line) => {
            println!(
                "{{\"lane\":\"{}\",\"threads\":{},\"preset\":{},\"filter\":\"{}\",\
                 \"direction\":\"{}\",\"bytes_in\":{},\"bytes_out\":{},\"crc32\":\"{}\",\
                 \"inproc_seconds\":{:.6},\"peak_alloc_bytes\":{},\"input_buffered_bytes\":{}}}",
                shot.lane,
                shot.threads,
                shot.preset,
                shot.filter.as_deref().unwrap_or(""),
                if shot.encode || shot.lane == "encode" {
                    "encode"
                } else {
                    "decode"
                },
                line.bytes_in,
                line.bytes_out,
                line.crc32.map(|c| format!("{c:08x}")).unwrap_or_default(),
                line.seconds,
                line.peak_alloc,
                line.input_buffered,
            );
            0
        }
        Err(e) => {
            eprintln!("lzma-bench: --shot {} {}: {e}", shot.lane, path.display());
            1
        }
    }
}

/// The build's own facts, for the report header. The CRC tier is the one
/// `crc-fast` picked at run time on this machine; the crate's CRCs are
/// `crc-fast`'s, at the version the lockfile pins for both. The commit and
/// dirty flag are the checkout's when this binary was built (`build.rs`),
/// empty and null where git could not say, so the harness can refuse a binary
/// that is not the source it reports.
fn info() -> String {
    let features: Vec<&str> = [
        ("sse4.2", cfg!(target_feature = "sse4.2")),
        ("avx2", cfg!(target_feature = "avx2")),
        ("avx512f", cfg!(target_feature = "avx512f")),
        ("pclmulqdq", cfg!(target_feature = "pclmulqdq")),
        ("neon", cfg!(target_feature = "neon")),
        ("aes", cfg!(target_feature = "aes")),
        ("sha3", cfg!(target_feature = "sha3")),
        ("sve2", cfg!(target_feature = "sve2")),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    let features = features
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"lane\":\"info\",\"lzma_turbo\":\"{}\",\"asm_loop\":{},\"target_arch\":\"{}\",\
         \"target_os\":\"{}\",\"crc32_tier\":\"{}\",\"crc64_tier\":\"{}\",\
         \"sha256_backend\":\"{}\",\"compile_target_features\":[{features}],\
         \"build_commit\":\"{}\",\"build_dirty\":{}}}",
        lzma_turbo::VERSION,
        lzma_turbo::ASM_LOOP,
        std::env::consts::ARCH,
        std::env::consts::OS,
        crc_fast::get_calculator_target(crc_fast::CrcAlgorithm::Crc32IsoHdlc),
        crc_fast::get_calculator_target(crc_fast::CrcAlgorithm::Crc64Xz),
        lzma_turbo::crypto::SHA256_BACKEND,
        env!("LZMA_BENCH_BUILD_COMMIT"),
        match env!("LZMA_BENCH_BUILD_DIRTY") {
            "" => "null",
            flag => flag,
        },
    )
}

fn measure(shot: &Shot, path: &Path) -> Result<Line, String> {
    let bytes_in = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    match shot.lane.as_str() {
        "lzma1" => {
            let file = open(path)?;
            read_all(bytes_in, shot.verify, || {
                LzmaReader::with_memory_limit(file, u64::MAX).map_err(|e| e.to_string())
            })
        }
        "lzma2" => {
            let (prop, stream) = lzma2_stream(path)?;
            read_all(bytes_in, shot.verify, || {
                Lzma2Reader::with_memory_limit(stream, prop, u64::MAX).map_err(|e| e.to_string())
            })
        }
        "lzma2-mt" => {
            let (prop, stream) = lzma2_stream(path)?;
            let opts = Lzma2MtOptions {
                threads: shot.threads.max(1),
                memory_limit: u64::MAX,
            };
            let dec = Lzma2ParallelDecoder::new(prop, &opts).map_err(|e| e.to_string())?;
            let mut sink = Sink::new(shot.verify);
            let base = alloc_watch_reset();
            let t0 = Instant::now();
            let bytes_out = dec.decode(stream, &mut sink).map_err(|e| e.to_string())?;
            Ok(Line {
                bytes_in,
                bytes_out,
                crc32: sink.crc.map(|mut c| c.finish()),
                seconds: t0.elapsed().as_secs_f64(),
                peak_alloc: alloc_watch_peak(base),
                input_buffered: 0,
            })
        }
        "adaptive" => adaptive(shot, path, bytes_in),
        "rust2-mt" => {
            let (prop, stream) = lzma2_stream(path)?;
            let dict = dict_size_from_prop(prop);
            let threads = u32::try_from(shot.threads.max(1)).map_err(|e| e.to_string())?;
            read_all(bytes_in, shot.verify, || {
                Ok(lzma_rust2::Lzma2ReaderMt::new(stream, dict, None, threads))
            })
        }
        "xz" => {
            let file = open(path)?;
            read_all(bytes_in, shot.verify, || Ok(XzReader::new(file)))
        }
        "xz-par" => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let opts = XzOptions::default().with_threads(shot.threads.max(1));
            read_all(bytes_in, shot.verify, || {
                XzParallelReader::with_options(file, opts).map_err(|e| e.to_string())
            })
        }
        "liblzma" => {
            let file = open(path)?;
            let threads = u32::try_from(shot.threads.max(1)).map_err(|e| e.to_string())?;
            read_all(bytes_in, shot.verify, || {
                let r: Box<dyn Read> = if threads > 1 {
                    let stream = liblzma::stream::MtStreamBuilder::new()
                        .threads(threads)
                        .memlimit_stop(u64::MAX)
                        .memlimit_threading(u64::MAX)
                        .decoder()
                        .map_err(|e| e.to_string())?;
                    Box::new(liblzma::read::XzDecoder::new_stream(file, stream))
                } else {
                    Box::new(liblzma::read::XzDecoder::new_multi_decoder(file))
                };
                Ok(r)
            })
        }
        "encode" => encode(shot, path, bytes_in),
        "filter" => filter(shot, path, bytes_in),
        other => Err(format!("unknown shot lane {other:?}")),
    }
}

/// The size of a piece the adaptive lane hands over: what a reader pulling
/// its own input in pieces it gives away would read at a time.
const ADAPTIVE_PIECE: usize = 4 << 20;
/// The smallest read the adaptive shot makes under a tight limit.
const ADAPTIVE_PIECE_MIN: usize = 64 << 10;

/// `Lzma2AdaptiveDecoder` the way a consumer pulling its own input drives it:
/// read a piece, hand it over whole, drain, and when the decoder refuses the
/// piece, drain and wait for a worker until it takes it. The pieces it is
/// done with come back through `reclaim_piece`, so the reader's footprint is
/// the pieces in flight and the one in its hand.
///
/// Chasing is off: the stream is on disk, so a run at the cursor is short
/// only because the rest of it has not been handed over yet.
fn adaptive(shot: &Shot, path: &Path, bytes_in: u64) -> Result<Line, String> {
    let (prop, mut stream) = lzma2_stream(path)?;
    let opts = Lzma2MtOptions {
        threads: shot.threads.max(1),
        memory_limit: shot.memory_limit,
    };
    let mut sink = Sink::new(shot.verify);
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts).map_err(|e| e.to_string())?;
    dec.set_chase(false);
    // The decoder charges a piece its allocation and admits it by that, so a
    // read sized past the limit would never be taken. Under a tight limit the
    // reads come down to a quarter of it, and never below a small floor.
    let piece_len = usize::try_from(shot.memory_limit / 4)
        .unwrap_or(usize::MAX)
        .clamp(ADAPTIVE_PIECE_MIN, ADAPTIVE_PIECE);
    let mut hand: Option<Vec<u8>> = None;
    let mut eof = false;
    let mut write_err = None;
    loop {
        if hand.is_none() && !eof {
            let mut piece = dec.reclaim_piece().unwrap_or_default();
            piece.clear();
            piece.reserve_exact(piece_len);
            piece.resize(piece_len, 0);
            let mut filled = 0;
            while filled < piece.len() {
                match stream
                    .read(&mut piece[filled..])
                    .map_err(|e| e.to_string())?
                {
                    0 => break,
                    n => filled += n,
                }
            }
            piece.truncate(filled);
            if filled < piece_len {
                eof = true;
            }
            if filled != 0 {
                hand = Some(piece);
            }
        }
        if let Some(piece) = hand.take() {
            hand = dec.feed_owned(piece).map_err(|e| e.to_string())?;
        }
        if eof && hand.is_none() {
            dec.end_of_input();
        }
        let status = dec
            .drain_upto(OUT_CHUNK, |_, b| {
                if let Err(e) = sink.write_all(b) {
                    write_err.get_or_insert(e);
                }
            })
            .map_err(|e| e.to_string())?;
        if let Some(e) = write_err.take() {
            return Err(e.to_string());
        }
        match status {
            DrainStatus::Finished => break,
            DrainStatus::Progress => {}
            DrainStatus::NeedsMoreInput => {
                // Refused, or nothing left to read: the only thing that can
                // change either is a worker finishing.
                if (hand.is_some() || eof) && !dec.wait_for_worker() && hand.is_some() {
                    return Err("the decoder refused a piece with nothing in flight".into());
                }
            }
        }
    }
    let seconds = t0.elapsed().as_secs_f64();
    eprintln!("ADAPTIVE_LEDGER {:?}", dec.ledger());
    drop(dec);
    Ok(Line {
        bytes_in,
        bytes_out: sink.written,
        crc32: sink.crc.map(|mut c| c.finish()),
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: 0,
    })
}

fn open(path: &Path) -> Result<BufReader<File>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    Ok(BufReader::with_capacity(IN_CHUNK, file))
}

/// Builds a reader inside the timed region, as a consumer would, and drains
/// it into a sink that counts, and hashes as well when `verify` is set.
fn read_all<R: Read>(
    bytes_in: u64,
    verify: bool,
    build: impl FnOnce() -> Result<R, String>,
) -> Result<Line, String> {
    let mut buf = vec![0u8; OUT_CHUNK];
    let mut sink = Sink::new(verify);
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut reader = build()?;
    let mut bytes_out = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes_out += n as u64;
        sink.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    let seconds = t0.elapsed().as_secs_f64();
    drop(reader);
    Ok(Line {
        bytes_in,
        bytes_out,
        crc32: sink.crc.map(|mut c| c.finish()),
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: 0,
    })
}

/// The raw LZMA2 stream of a single-block `.xz` or a single-folder `.7z`, as
/// a reader positioned on its first byte, with its dictionary property byte.
fn lzma2_stream(path: &Path) -> Result<(u8, std::io::Take<BufReader<File>>), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let (prop, offset, len) = if ext == "7z" {
        let mut sig = [0u8; 32];
        file.read_exact(&mut sig).map_err(|e| e.to_string())?;
        let (start, len) = crate::sevenz::header_range(&sig).ok_or("not a .7z file")?;
        let mut header = vec![0u8; usize::try_from(len).map_err(|e| e.to_string())?];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        let ps =
            crate::sevenz::pack_stream_in_header(&header).ok_or("not a single-folder LZMA2 .7z")?;
        (ps.dict_prop, ps.offset, ps.packed_len)
    } else {
        // A block header is at most 1024 bytes and follows the 12-byte stream
        // header, so the first few KiB hold everything the walk needs.
        let mut head = vec![0u8; 4096];
        let n = read_up_to(&mut file, &mut head)?;
        head.truncate(n);
        let (prop, rest) = crate::xz_lzma2_block(&head).ok_or("not a single-block LZMA2 .xz")?;
        let offset = (head.len() - rest.len()) as u64;
        // The LZMA2 stream ends at its own end marker; what follows it (the
        // check, the index) is never read.
        (prop, offset, u64::MAX)
    };
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    Ok((prop, BufReader::with_capacity(IN_CHUNK, file).take(len)))
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]).map_err(|e| e.to_string())? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// `.xz` filter-flags for a filter name, for the writer.
fn filter_flags(name: &str) -> Result<FilterFlags, String> {
    if let Some(dist) = name.strip_prefix("delta:") {
        let dist: u16 = dist.parse().map_err(|_| "delta:N takes a distance")?;
        if !(1..=256).contains(&dist) {
            return Err("delta distance is 1 to 256".into());
        }
        let prop = u8::try_from(dist - 1).map_err(|e| e.to_string())?;
        return FilterFlags::new(lzma_turbo::xz::FILTER_DELTA, &[prop]).map_err(|e| e.to_string());
    }
    let kind = bcj_kind(name)?;
    FilterFlags::new(kind.filter_id(), &[]).map_err(|e| e.to_string())
}

fn bcj_kind(name: &str) -> Result<lzma_turbo::filters::bcj::BcjKind, String> {
    use lzma_turbo::filters::bcj::BcjKind;
    Ok(match name {
        "x86" => BcjKind::X86,
        "arm" => BcjKind::Arm,
        "armthumb" => BcjKind::ArmThumb,
        "arm64" => BcjKind::Arm64,
        "ppc" | "powerpc" => BcjKind::Ppc,
        "sparc" => BcjKind::Sparc,
        "ia64" => BcjKind::Ia64,
        "riscv" => BcjKind::RiscV,
        other => return Err(format!("unknown filter {other:?}")),
    })
}

/// `XzWriter` at a preset, streaming the input through it into a counting
/// sink. Only counting: `xz`'s output goes to a sink that counts it too, and
/// no one checks an encoder's CRC, so hashing here would time work only this
/// side does. At one thread it writes one solid block, as `xz -T1` does; above one
/// it cuts blocks at the size `xz -T<n>` would, so the two are comparable.
fn encode(shot: &Shot, path: &Path, bytes_in: u64) -> Result<Line, String> {
    // `xz -N`'s own settings, not 7-Zip's level N: the oracle row runs `xz -N`,
    // so the size column only means something when both sides use the same
    // dictionary, match finder and depth.
    let props = LzmaEncProps::xz_preset(shot.preset, false)
        .map_err(|e| e.to_string())?
        .with_num_threads(shot.mf_threads.max(1));
    let mut input = open(path)?;
    let mut buf = vec![0u8; IN_CHUNK];
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut w = XzWriter::new(Sink::new(false), &props).map_err(|e| e.to_string())?;
    w.set_check(CheckType::Crc64).map_err(|e| e.to_string())?;
    if let Some(f) = &shot.filter {
        w.set_filters(&[filter_flags(f)?])
            .map_err(|e| e.to_string())?;
    }
    if shot.threads > 1 {
        w.set_block_size(crate::encode::xz_mt_block_size(
            props.normalized().dict_size,
        ));
        w.set_threads(shot.threads);
    }
    loop {
        let n = input.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    let sink = w.finish().map_err(|e| e.to_string())?;
    let seconds = t0.elapsed().as_secs_f64();
    Ok(Line {
        bytes_in,
        bytes_out: sink.written,
        crc32: None,
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: 0,
    })
}

/// One converter over the whole file in memory: the converters are in-place
/// and have no streaming state worth timing apart from the pass itself.
///
/// A decode shot first encodes the buffer, untimed, so the timed pass undoes
/// real conversions and its CRC must equal the source file's: the harness
/// checks that, which makes every decode row a round trip as well.
fn filter(shot: &Shot, path: &Path, bytes_in: u64) -> Result<Line, String> {
    let name = shot
        .filter
        .as_deref()
        .ok_or("--shot filter needs --filter")?;
    let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
    let buffered = data.len() as u64;

    if name == "bcj2" {
        use lzma_turbo::filters::bcj2;
        let base = alloc_watch_reset();
        if shot.encode {
            let t0 = Instant::now();
            let s = bcj2::encode_to_streams(&data);
            let seconds = t0.elapsed().as_secs_f64();
            let mut crc = crate::Crc32::new();
            for part in [&s.main, &s.call, &s.jump, &s.rc] {
                crc.update(part);
            }
            let out = (s.main.len() + s.call.len() + s.jump.len() + s.rc.len()) as u64;
            return Ok(Line {
                bytes_in,
                bytes_out: out,
                crc32: Some(crc.finish()),
                seconds,
                peak_alloc: alloc_watch_peak(base),
                input_buffered: buffered,
            });
        }
        let s = bcj2::encode_to_streams(&data);
        let len = data.len();
        drop(data);
        let base = alloc_watch_reset();
        let t0 = Instant::now();
        let out = bcj2::decode_to_vec(&s.main, &s.call, &s.jump, &s.rc, len)
            .map_err(|e| e.to_string())?;
        let seconds = t0.elapsed().as_secs_f64();
        let mut crc = crate::Crc32::new();
        crc.update(&out);
        return Ok(Line {
            bytes_in,
            bytes_out: out.len() as u64,
            crc32: Some(crc.finish()),
            seconds,
            peak_alloc: alloc_watch_peak(base),
            input_buffered: buffered,
        });
    }

    let convert = |data: &mut [u8], encode: bool| -> Result<(), String> {
        if let Some(dist) = name.strip_prefix("delta:") {
            let dist: usize = dist.parse().map_err(|_| "delta:N takes a distance")?;
            if !(1..=256).contains(&dist) {
                return Err("delta distance is 1 to 256".into());
            }
            let mut d = lzma_turbo::filters::delta::Delta::new((dist - 1) as u8)
                .map_err(|e| e.to_string())?;
            if encode {
                d.encode(data);
            } else {
                d.decode(data);
            }
            return Ok(());
        }
        let mut b =
            lzma_turbo::filters::bcj::Bcj::new(bcj_kind(name)?, 0).map_err(|e| e.to_string())?;
        if encode {
            b.encode(data);
        } else {
            b.decode(data);
        }
        Ok(())
    };
    if !shot.encode {
        convert(&mut data, true)?;
    }
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    convert(&mut data, shot.encode)?;
    let seconds = t0.elapsed().as_secs_f64();
    let mut crc = crate::Crc32::new();
    crc.update(&data);
    Ok(Line {
        bytes_in,
        bytes_out: data.len() as u64,
        crc32: Some(crc.finish()),
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: buffered,
    })
}

/// Where a shot's output goes: counted, as the harness counts `xz`'s or
/// sends it to the null device, and hashed only when a CRC was asked for.
struct Sink {
    crc: Option<Crc32>,
    written: u64,
}

impl Sink {
    fn new(hash: bool) -> Self {
        Sink {
            crc: hash.then(Crc32::new),
            written: 0,
        }
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(crc) = &mut self.crc {
            crc.update(buf);
        }
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch file of its own for each test, removed when dropped.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str, data: &[u8]) -> Self {
            let path =
                std::env::temp_dir().join(format!("lzma-bench-{}-{name}", std::process::id()));
            std::fs::write(&path, data).expect("write scratch file");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn payload() -> Vec<u8> {
        (0..300_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 23) as u8)
            .collect()
    }

    fn shot(lane: &str) -> Shot {
        Shot {
            lane: lane.into(),
            threads: 1,
            preset: 1,
            mf_threads: 1,
            filter: None,
            encode: false,
            verify: false,
            memory_limit: u64::MAX,
        }
    }

    /// The encode shot counts its output and hashes none of it: `xz`'s
    /// output goes to a counting sink, so a hash here is work only this side
    /// would be timed doing.
    #[test]
    fn an_encode_shot_counts_and_does_not_hash() {
        let data = payload();
        let input = Scratch::new("encode.bin", &data);
        let line = measure(&shot("encode"), &input.0).expect("encode shot");
        assert_eq!(line.crc32, None);

        let props = LzmaEncProps::xz_preset(1, false).expect("preset");
        let mut w = XzWriter::new(Vec::new(), &props).expect("writer");
        w.set_check(CheckType::Crc64).expect("check");
        w.write_all(&data).expect("write");
        let want = w.finish().expect("finish").len() as u64;
        assert_eq!(line.bytes_out, want);
    }

    /// A decoder shot counts its output unless asked to verify it, and then
    /// reports the CRC of exactly what it decoded.
    #[test]
    fn a_decode_shot_hashes_only_when_verifying() {
        let data = payload();
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let xz = lzma_turbo::encode_xz(&data, &props, CheckType::Crc64, 0).expect("encode");
        let input = Scratch::new("decode.xz", &xz);
        let mut want = Crc32::new();
        want.update(&data);
        let want = want.finish();

        for lane in ["xz", "xz-par", "lzma2", "lzma2-mt", "adaptive"] {
            let mut s = shot(lane);
            let line = measure(&s, &input.0).expect("decode shot");
            assert_eq!(line.bytes_out, data.len() as u64, "{lane}");
            assert_eq!(line.crc32, None, "{lane}: hashed without --verify");

            s.verify = true;
            let line = measure(&s, &input.0).expect("verify shot");
            assert_eq!(line.bytes_out, data.len() as u64, "{lane}");
            assert_eq!(line.crc32, Some(want), "{lane}: --verify");
        }
    }
}
