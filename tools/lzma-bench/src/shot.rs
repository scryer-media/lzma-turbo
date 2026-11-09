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
    BLOCK_SIZE_AUTO, DrainStatus, Lzma2AdaptiveDecoder, Lzma2Encoder, Lzma2MtOptions,
    Lzma2ParallelDecoder, Lzma2Reader, LzmaEncProps, LzmaReader, XzWriter,
};

use crate::{Crc32, OUT_CHUNK, alloc_watch_peak, alloc_watch_reset, dict_size_from_prop};

const IN_CHUNK: usize = 1 << 20;

/// What a decode lane reads its output in. xz moves data in pieces of this
/// class and the crate's readers fill a 64 KiB input buffer of their own, so
/// a larger piece would only add to the process's peak RSS what the crate
/// does not hold: the 1 MiB this used to be, with a 1 MiB `BufReader` in
/// front of the crate's own buffer, put 1 MiB on the single-stream rows'
/// peak.
const READ_CHUNK: usize = 1 << 16;

pub const HELP: &str = "\
  --shot LANE    run one measurement and print one JSON line, for a harness
                 that measures each contender as its own process:
                   info           build facts: version, decode loop, CRC tier
                   lzma1          LzmaReader over a .lzma file
                   lzma2          Lzma2Reader over the LZMA2 stream of a
                                  single-block .xz or a single-folder .7z
                   lzma2-mt       Lzma2ParallelDecoder, --threads N
                   lzma2-mt-at    Lzma2ParallelDecoder::decode_at on the file, --threads N
                   adaptive       Lzma2AdaptiveDecoder fed the stream in
                                  4 MiB pieces it takes ownership of,
                                  --threads N, optional --memory-limit B
                   rust2-mt       lzma-rust2 Lzma2ReaderMt, --threads N
                   rust2-lzma1    lzma-rust2 LzmaReader over a .lzma file
                   rust2-lzma2    lzma-rust2 Lzma2Reader, the lzma2 lane's input
                   rust2-xz       lzma-rust2 XzReader over a whole .xz file
                   rust2-xz-mt    lzma-rust2 XzReaderMt, --threads N
                   lzmars-lzma1   lzma-rs over a .lzma file
                   lzmars-lzma2   lzma-rs, the lzma2 lane's input
                   lzmars-xz      lzma-rs over a whole .xz file
                   xz             XzReader over a whole .xz file
                   xz-par         XzParallelReader, --threads N
                   liblzma        the liblzma crate (C), --threads N
                   liblzma-lzma1  the liblzma crate over a .lzma file
                   encode-7z      Lzma2Encoder at 7-Zip's level P with N
                                  total threads, as `7zz a -mx=P -mmt=N`
                                  sets it up, writing the raw stream to
                                  --out FILE and taking the input's CRC-32
                   encode         XzWriter at --preset P, --threads N,
                                  optional --filter F
                   rust2-encode   lzma-rust2 XzWriter (XzWriterMt above one
                                  thread) at --preset P, --threads N
                   liblzma-encode the liblzma crate's .xz encoder at
                                  --preset P, --threads N
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
    /// Where an encode shot that writes a file puts it (`encode-7z`).
    pub out: Option<std::path::PathBuf>,
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
                if shot.encode || shot.lane == "encode" || shot.lane == "encode-7z" {
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
/// `crc-turbo` picked at run time on this machine; the crate's CRCs are
/// `crc-turbo`'s, at the version the lockfile pins for both. The commit and
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
        crc_turbo::get_calculator_target(crc_turbo::CrcAlgorithm::Crc32IsoHdlc),
        crc_turbo::get_calculator_target(crc_turbo::CrcAlgorithm::Crc64Xz),
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
            // `Lzma2Reader` buffers its own input.
            let (prop, stream) = lzma2_stream_unbuffered(path)?;
            read_all(bytes_in, shot.verify, || {
                Lzma2Reader::with_memory_limit(stream, prop, u64::MAX).map_err(|e| e.to_string())
            })
        }
        "lzma2-mt" => {
            // The decoder reads its input in 256 KiB pieces of its own, as
            // 7-Zip's does, so a `BufReader` in front would only copy every
            // byte twice inside the decode's one serial stage.
            let (prop, stream) = lzma2_stream_unbuffered(path)?;
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
        "lzma2-mt-at" => {
            // The same decode from a file read at offsets: every run is read
            // by the thread that decodes it, not one after another.
            let (prop, stream) = lzma2_stream_unbuffered(path)?;
            let len = stream.limit();
            let mut file = stream.into_inner();
            let offset = file.stream_position().map_err(|e| e.to_string())?;
            let opts = Lzma2MtOptions {
                threads: shot.threads.max(1),
                memory_limit: u64::MAX,
            };
            let dec = Lzma2ParallelDecoder::new(prop, &opts).map_err(|e| e.to_string())?;
            let mut sink = Sink::new(shot.verify);
            let base = alloc_watch_reset();
            let t0 = Instant::now();
            let bytes_out = dec
                .decode_at(&file, offset, len, &mut sink)
                .map_err(|e| e.to_string())?;
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
            // The reader buffers its own input, as `xz-par` does.
            let file = File::open(path).map_err(|e| e.to_string())?;
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
        "rust2-lzma1" => {
            let file = open(path)?;
            read_all(bytes_in, shot.verify, || {
                lzma_rust2::LzmaReader::new_mem_limit(file, u32::MAX, None)
                    .map_err(|e| e.to_string())
            })
        }
        "rust2-lzma2" => {
            let (prop, stream) = lzma2_stream(path)?;
            let dict = dict_size_from_prop(prop);
            read_all(bytes_in, shot.verify, || {
                Ok(lzma_rust2::Lzma2Reader::new(stream, dict, None))
            })
        }
        "rust2-xz" => {
            let file = open(path)?;
            read_all(bytes_in, shot.verify, || {
                Ok(lzma_rust2::XzReader::new(file, true))
            })
        }
        "rust2-xz-mt" => {
            let file = open(path)?;
            let threads = u32::try_from(shot.threads.max(1)).map_err(|e| e.to_string())?;
            read_all(bytes_in, shot.verify, || {
                lzma_rust2::XzReaderMt::new(file, true, threads).map_err(|e| e.to_string())
            })
        }
        "lzmars-lzma1" => {
            let mut file = open(path)?;
            push_all(bytes_in, shot.verify, |sink| {
                lzma_rs::lzma_decompress(&mut file, sink).map_err(|e| e.to_string())
            })
        }
        "lzmars-lzma2" => {
            let (_, mut stream) = lzma2_stream(path)?;
            let mut stream = BufReader::with_capacity(IN_CHUNK, &mut stream);
            push_all(bytes_in, shot.verify, |sink| {
                lzma_rs::lzma2_decompress(&mut stream, sink).map_err(|e| e.to_string())
            })
        }
        "lzmars-xz" => {
            let mut file = open(path)?;
            push_all(bytes_in, shot.verify, |sink| {
                lzma_rs::xz_decompress(&mut file, sink).map_err(|e| e.to_string())
            })
        }
        "liblzma-lzma1" => {
            let file = open(path)?;
            read_all(bytes_in, shot.verify, || {
                let stream = liblzma::stream::Stream::new_lzma_decoder(u64::MAX)
                    .map_err(|e| e.to_string())?;
                Ok(liblzma::read::XzDecoder::new_stream(file, stream))
            })
        }
        "encode" => encode(shot, path, bytes_in),
        "encode-7z" => encode_7z(shot, path, bytes_in),
        "rust2-encode" | "liblzma-encode" => encode_reference(shot, path, bytes_in),
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
    // reads come down to a quarter of it, and not below a small floor - but
    // never past the limit itself, which the floor would be under a limit
    // smaller than it.
    let limit = usize::try_from(shot.memory_limit).unwrap_or(usize::MAX);
    let piece_len = (limit / 4)
        .clamp(ADAPTIVE_PIECE_MIN, ADAPTIVE_PIECE)
        .min(limit)
        .max(1);
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
    let mut buf = vec![0u8; READ_CHUNK];
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

/// [`read_all`] for a decoder that pushes its output into a writer rather
/// than being read from: it decodes into the sink inside the timed region.
fn push_all(
    bytes_in: u64,
    verify: bool,
    decode: impl FnOnce(&mut Sink) -> Result<(), String>,
) -> Result<Line, String> {
    let mut sink = Sink::new(verify);
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    decode(&mut sink)?;
    let seconds = t0.elapsed().as_secs_f64();
    Ok(Line {
        bytes_in,
        bytes_out: sink.written,
        crc32: sink.crc.map(|mut c| c.finish()),
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: 0,
    })
}

/// The raw LZMA2 stream of a single-block `.xz` or a single-folder `.7z`, as
/// a reader positioned on its first byte, with its dictionary property byte.
fn lzma2_stream(path: &Path) -> Result<(u8, std::io::Take<BufReader<File>>), String> {
    let (prop, stream) = lzma2_stream_unbuffered(path)?;
    let len = stream.limit();
    Ok((
        prop,
        BufReader::with_capacity(IN_CHUNK, stream.into_inner()).take(len),
    ))
}

/// [`lzma2_stream`] with no buffer in front of the file, for a reader that
/// buffers its own input.
fn lzma2_stream_unbuffered(path: &Path) -> Result<(u8, std::io::Take<File>), String> {
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
    Ok((prop, file.take(len)))
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

/// The lzma-rust2 or liblzma `.xz` encoder at `xz -<P>`'s preset, streaming
/// the input through it into a counting sink, as [`encode`] does. CRC-64 as
/// `xz` writes. Above one thread both cut blocks at the size `xz -T<n>` and
/// this crate's writer use, so every row compares the same block layout.
fn encode_reference(shot: &Shot, path: &Path, bytes_in: u64) -> Result<Line, String> {
    use liblzma::stream::{Check, MtStreamBuilder, Stream};
    use lzma_rust2::{XzOptions, XzWriter as Rust2Writer, XzWriterMt as Rust2WriterMt};
    let dict = LzmaEncProps::xz_preset(shot.preset, false)
        .map_err(|e| e.to_string())?
        .normalized()
        .dict_size;
    let block = crate::encode::xz_mt_block_size(dict);
    let threads = u32::try_from(shot.threads.max(1)).map_err(|e| e.to_string())?;
    let mut input = open(path)?;
    let mut buf = vec![0u8; IN_CHUNK];
    let err = |e: std::io::Error| e.to_string();
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let bytes_out = if shot.lane == "rust2-encode" {
        let mut opts = XzOptions::with_preset(shot.preset);
        opts.set_check_sum_type(lzma_rust2::CheckType::Crc64);
        if threads > 1 {
            opts.set_block_size(std::num::NonZeroU64::new(block));
            let mut w = Rust2WriterMt::new(Sink::new(false), opts, threads).map_err(err)?;
            pump(&mut input, &mut buf, &mut w)?;
            w.finish().map_err(err)?.written
        } else {
            let mut w = Rust2Writer::new(Sink::new(false), opts).map_err(err)?;
            pump(&mut input, &mut buf, &mut w)?;
            w.finish().map_err(err)?.written
        }
    } else {
        let stream = if threads > 1 {
            MtStreamBuilder::new()
                .threads(threads)
                .preset(shot.preset)
                .block_size(block)
                .check(Check::Crc64)
                .encoder()
        } else {
            Stream::new_easy_encoder(shot.preset, Check::Crc64)
        }
        .map_err(|e| e.to_string())?;
        let mut w = liblzma::write::XzEncoder::new_stream(Sink::new(false), stream);
        pump(&mut input, &mut buf, &mut w)?;
        w.finish().map_err(err)?.written
    };
    let seconds = t0.elapsed().as_secs_f64();
    Ok(Line {
        bytes_in,
        bytes_out,
        crc32: None,
        seconds,
        peak_alloc: alloc_watch_peak(base),
        input_buffered: 0,
    })
}

/// Copies `input` into `w` through `buf`.
fn pump(input: &mut impl Read, buf: &mut [u8], w: &mut impl Write) -> Result<(), String> {
    loop {
        let n = input.read(buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
}

/// The work `7zz a -t7z -m0=lzma2 -mx=P -mmt=N` does for one file, without
/// the archive around it: the raw LZMA2 stream written to a file, and the
/// input's CRC-32, which a `.7z` stores and 7-Zip takes as it reads.
///
/// The settings are 7-Zip's level `P`, not `xz -P`'s. 7-Zip hands LZMA2 the
/// thread count as a total (`numTotalThreads`), leaves the block size
/// automatic, and passes the file's size both as `reduceSize` and as the
/// expected data size. The finder's thread count is the SDK's default, two for
/// a binary tree outside fast mode (level 5 and up) and one otherwise, which
/// `Lzma2EncProps_Normalize` drops back to one when the total is one.
fn encode_7z(shot: &Shot, path: &Path, bytes_in: u64) -> Result<Line, String> {
    struct In {
        file: File,
        crc: Crc32,
    }
    impl lzma_turbo::SeqInStream for In {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, lzma_turbo::Error> {
            let n = self.file.read(buf).map_err(|_| lzma_turbo::Error::Read)?;
            self.crc.update(&buf[..n]);
            Ok(n)
        }
    }
    struct Out {
        file: File,
        written: u64,
    }
    impl lzma_turbo::SeqOutStream for Out {
        fn supports_streamed_blocks(&self) -> bool {
            true
        }
        fn write(&mut self, data: &[u8]) -> Result<(), lzma_turbo::Error> {
            self.file
                .write_all(data)
                .map_err(|_| lzma_turbo::Error::Write)?;
            self.written += data.len() as u64;
            Ok(())
        }
    }

    let out_path = shot
        .out
        .as_deref()
        .ok_or("--shot encode-7z needs --out FILE")?;
    let threads = shot.threads.max(1);
    let finder_threads = if threads > 1 && shot.preset >= 5 {
        2
    } else {
        1
    };
    let props = LzmaEncProps::new()
        .with_level(shot.preset)
        .with_reduce_size(bytes_in)
        .with_num_threads(finder_threads);
    let mut input = In {
        file: File::open(path).map_err(|e| e.to_string())?,
        crc: Crc32::new(),
    };
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut out = Out {
        file: File::create(out_path).map_err(|e| e.to_string())?,
        written: 0,
    };
    let mut enc = Lzma2Encoder::new(&props).map_err(|e| e.to_string())?;
    enc.set_block_size(BLOCK_SIZE_AUTO);
    enc.set_total_threads(threads);
    enc.set_data_size(bytes_in);
    enc.encode_mt(&mut input, &mut out)
        .map_err(|e| e.to_string())?;
    out.file.flush().map_err(|e| e.to_string())?;
    let crc = input.crc.finish();
    let seconds = t0.elapsed().as_secs_f64();
    std::hint::black_box(crc);
    Ok(Line {
        bytes_in,
        bytes_out: out.written,
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
            out: None,
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

        for lane in [
            "xz",
            "xz-par",
            "lzma2",
            "lzma2-mt",
            "adaptive",
            "rust2-mt",
            "rust2-lzma2",
            "rust2-xz",
            "rust2-xz-mt",
            "lzmars-lzma2",
            "lzmars-xz",
            "liblzma",
        ] {
            assert_decodes(lane, &input.0, data.len() as u64, want);
        }
    }

    /// Every LZMA1 lane decodes the same `.lzma` file to the source.
    #[test]
    fn every_lzma1_lane_decodes() {
        let data = payload();
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let lzma = lzma_turbo::encode_lzma_alone(&data, &props).expect("encode");
        let input = Scratch::new("decode.lzma", &lzma);
        let mut want = Crc32::new();
        want.update(&data);
        let want = want.finish();
        for lane in ["lzma1", "rust2-lzma1", "lzmars-lzma1", "liblzma-lzma1"] {
            assert_decodes(lane, &input.0, data.len() as u64, want);
        }
    }

    fn assert_decodes(lane: &str, path: &Path, len: u64, want: u32) {
        let mut s = shot(lane);
        s.threads = 2;
        let line = measure(&s, path).expect("decode shot");
        assert_eq!(line.bytes_out, len, "{lane}");
        assert_eq!(line.crc32, None, "{lane}: hashed without --verify");

        s.verify = true;
        let line = measure(&s, path).expect("verify shot");
        assert_eq!(line.bytes_out, len, "{lane}");
        assert_eq!(line.crc32, Some(want), "{lane}: --verify");
    }

    /// The reference encode shots count exactly what each library writes for
    /// the same settings, on one thread and on two.
    #[test]
    fn the_reference_encode_shots_count_the_library_output() {
        let data = payload();
        let input = Scratch::new("encode-ref.bin", &data);
        let block = crate::encode::xz_mt_block_size(
            LzmaEncProps::xz_preset(1, false)
                .expect("preset")
                .normalized()
                .dict_size,
        );
        for threads in [1usize, 2] {
            let mut s = shot("rust2-encode");
            s.threads = threads;
            let line = measure(&s, &input.0).expect("rust2 encode");
            let mut opts = lzma_rust2::XzOptions::with_preset(1);
            opts.set_check_sum_type(lzma_rust2::CheckType::Crc64);
            let want = if threads > 1 {
                opts.set_block_size(std::num::NonZeroU64::new(block));
                let mut w = lzma_rust2::XzWriterMt::new(Vec::new(), opts, 2).expect("writer");
                w.write_all(&data).expect("write");
                w.finish().expect("finish")
            } else {
                let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).expect("writer");
                w.write_all(&data).expect("write");
                w.finish().expect("finish")
            };
            assert_eq!(line.bytes_out, want.len() as u64, "rust2 t{threads}");
            assert_eq!(line.crc32, None);

            let mut s = shot("liblzma-encode");
            s.threads = threads;
            let line = measure(&s, &input.0).expect("liblzma encode");
            let want = liblzma::encode_all(&data[..], 1).expect("liblzma");
            if threads == 1 {
                assert_eq!(line.bytes_out, want.len() as u64, "liblzma t1");
            } else {
                assert!(line.bytes_out > 0, "liblzma t2");
            }
        }
    }

    /// Under a limit smaller than the read floor the adaptive shot still
    /// reads pieces the decoder can take, and decodes the stream.
    #[test]
    fn an_adaptive_shot_under_a_limit_below_the_read_floor_decodes() {
        let data = payload();
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let xz = lzma_turbo::encode_xz(&data, &props, CheckType::Crc64, 0).expect("encode");
        let input = Scratch::new("small-limit.xz", &xz);
        let mut s = shot("adaptive");
        s.threads = 2;
        s.memory_limit = 48 << 10;
        let line = measure(&s, &input.0).expect("adaptive shot under 48 KiB");
        assert_eq!(line.bytes_out, data.len() as u64);
    }
}
