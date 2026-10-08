//! Decode-throughput harness. See `docs/benchmarking.md`.
//!
//! ```text
//! lzma-bench [--runs N] [--no-oracles] <file.lzma|file.xz> ...
//! ```
//!
//! Decodes each input to a discard sink through the streaming API, reports
//! wall time and MiB/s of output, and prints a CRC32 of the output so parity
//! with the oracles can be checked rather than assumed. With oracles enabled
//! it times `7zz t -mmt=1`, `xz -dc -T1` and the reference C decoder
//! (`7lzma d`) on the same file in the same session and prints the ratio that
//! the acceptance gate is stated in.
//!
//! Containers: `.lzma` is the LZMA-alone format (13-byte header). `.xz` is
//! parsed just far enough to find a single LZMA2 block's filter property byte
//! and its compressed data. `.7z` is read by [`sevenz`], which finds the
//! LZMA2 stream of a single-file, single-folder archive and nothing else.
//! Anything more elaborate — multi-block xz, filter chains, real 7z folder
//! graphs — is out of scope for a harness that only needs one long LZMA2
//! stream to time.
//!
//! With `--threads` the LZMA2 lane runs multi-threaded instead, sweeping
//! thread counts and timing `7zz t -mmt=N` and lzma-rust2's `Lzma2ReaderMt`
//! at each of them. lzma-rust2 is a dependency of this harness only; the
//! library has none.

mod encode;
mod sevenz;
mod shot;
mod xz;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lzma_turbo::{
    Checksum, ChecksumPlan, DrainStatus, FinishMode, Lzma2AdaptiveDecoder, Lzma2Decoder,
    Lzma2MtOptions, Lzma2ParallelDecoder, LzmaAloneHeader, LzmaDecoder, Status,
};

const OUT_CHUNK: usize = 1 << 20;

const HELP: &str = "\
usage: lzma-bench [--runs N] [--no-oracles] [--threads LIST] [--checksum K] <file> ...

  inputs         .lzma, .xz, or a single-folder LZMA2 .7z
  --runs N       repetitions per decoder (default 3); the median is reported
  --no-oracles   time only this crate, skip 7zz / xz / 7lzma / lzma-rust2
  --portable     force the portable decode loop instead of the assembly one
  --index        print the stream's independently decodable runs and exit,
                 which is what bounds how far the parallel decode can scale
  --threads LIST multi-threaded LZMA2: a comma-separated list of thread counts,
                 where \"all\" means this machine's available parallelism.
                 \"--threads sweep\" is shorthand for 1,2,4,8,16,all
  --adaptive     time `Lzma2AdaptiveDecoder` on an on-disk stream as well as
                 the ring, at each `--threads` count: the shape a consumer
                 that pulls its own input uses
  --xz           treat the inputs as whole `.xz` files and time the container
                 layer instead of one LZMA2 stream: `XzReader` sequentially,
                 `XzParallelReader` at each `--threads` count, against
                 `xz -dc -T<n>`, `7zz t` and the liblzma crate
  --encode LIST  encode instead of decode: compress each input (raw bytes) to
                 `.xz` at each preset in the list, or \"sweep\" for 1,3,5,6,9,
                 and time it against `xz -T<n> -N` on the same data. Both the
                 time and the output size are reported, because one without
                 the other says nothing about a compressor. With `--threads`
                 the preset is run at each thread count, splitting into blocks
                 at the size `xz -T` would use so the two are comparable; at
                 one thread both sides write one solid block.
  --mf-threads N with `--encode`, the match finder's own thread count: 2 runs
                 the encoder's threaded match finder (C: `LzFindMt.c`), which
                 is a second thread per block coder and is independent of
                 `--threads`. The default, 1, is the single-threaded finder.
  --checksum K   have the decoder's own workers checksum their output:
                 none (default), crc32, crc64 or sha256. The CRCs are cut into
                 segments every 16 MiB, so the row also shows what splitting
                 costs. This is the measurement that matters for a consumer:
                 a checksum computed by the worker is parallel, one computed
                 by the sink runs inside the ring's serialised write section.
";

fn main() {
    let mut runs = 3usize;
    let mut oracles = true;
    let mut portable = false;
    let mut threads: Vec<usize> = Vec::new();
    let mut index = false;
    let mut xz_mode = false;
    let mut adaptive = false;
    let mut checksum = Checksum::None;
    let mut presets: Vec<u32> = Vec::new();
    let mut mf_threads = 1u32;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut shot_lane: Option<String> = None;
    let mut shot_preset = 6u32;
    let mut shot_filter: Option<String> = None;
    let mut shot_encode = false;
    let mut shot_verify = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--runs needs a number"));
            }
            "--no-oracles" | "--only-ours" => oracles = false,
            "--xz" => xz_mode = true,
            "--adaptive" => adaptive = true,
            "--encode" => {
                let v = args.next().unwrap_or_else(|| fail("--encode needs a list"));
                presets = encode::parse_presets(&v);
            }
            "--mf-threads" => {
                mf_threads = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--mf-threads needs a number"));
            }
            "--shot" => {
                shot_lane = Some(args.next().unwrap_or_else(|| fail("--shot needs a lane")));
            }
            "--preset" => {
                shot_preset = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|p| *p <= 9)
                    .unwrap_or_else(|| fail("--preset needs 0-9"));
            }
            "--filter" => {
                shot_filter = Some(args.next().unwrap_or_else(|| fail("--filter needs a name")));
            }
            "--direction" => {
                shot_encode = match args.next().as_deref() {
                    Some("encode") => true,
                    Some("decode") => false,
                    _ => fail("--direction takes encode or decode"),
                };
            }
            "--verify" => shot_verify = true,
            "--portable" => portable = true,
            "--index" => index = true,
            "--checksum" => {
                let v = args
                    .next()
                    .unwrap_or_else(|| fail("--checksum needs a kind"));
                checksum = match v.as_str() {
                    "none" => Checksum::None,
                    "crc32" => Checksum::Crc32,
                    "crc64" | "crc64xz" => Checksum::Crc64Xz,
                    "sha256" => Checksum::Sha256,
                    other => fail(&format!("unknown checksum {other}")),
                };
            }
            "--threads" => {
                let v = args
                    .next()
                    .unwrap_or_else(|| fail("--threads needs a list"));
                threads = parse_threads(&v);
            }
            "-h" | "--help" => {
                println!("{HELP}{}", shot::HELP);
                return;
            }
            other if other.starts_with('-') => fail(&format!("unknown option {other}")),
            other => files.push(PathBuf::from(other)),
        }
    }

    if let Some(lane) = shot_lane {
        // One measurement, one JSON line, nothing else on stdout.
        let shot = shot::Shot {
            lane,
            threads: threads.first().copied().unwrap_or(1),
            preset: shot_preset,
            mf_threads,
            filter: shot_filter,
            encode: shot_encode,
            verify: shot_verify,
        };
        std::process::exit(shot::run(&shot, files.first().map(PathBuf::as_path)));
    }

    if files.is_empty() {
        println!("{HELP}{}", shot::HELP);
        std::process::exit(2);
    }

    let loop_name = if portable || !lzma_turbo::ASM_LOOP {
        "portable"
    } else {
        "asm"
    };
    println!(
        "lzma-bench (lzma-turbo {}, {loop_name} loop), {runs} run(s), median",
        lzma_turbo::VERSION
    );

    for file in &files {
        if !presets.is_empty() {
            encode::bench(file, runs, oracles, &presets, &threads, mf_threads);
        } else if index {
            run_index(file);
        } else if adaptive {
            bench_adaptive(file, runs, &threads);
        } else if xz_mode {
            xz::bench(file, runs, oracles, &threads);
        } else if threads.is_empty() {
            bench_one(file, runs, oracles, portable);
        } else {
            bench_mt(file, runs, oracles, &threads, checksum);
        }
    }
}

/// Prints the run index of a stream: how many independently decodable runs it
/// has, and how big they are. A decode cannot use more threads than there are
/// runs, so this is the first thing to look at when a scaling curve flattens.
fn run_index(path: &Path) {
    let Ok(data) = std::fs::read(path) else {
        eprintln!("lzma-bench: cannot read {}", path.display());
        return;
    };
    let Ok((_, payload)) = lzma2_payload(path, &data) else {
        eprintln!("lzma-bench: {}: not an LZMA2 stream", path.display());
        return;
    };
    let mut scanner = lzma_turbo::Lzma2RunScanner::new();
    if let Err(e) = scanner.feed(payload) {
        eprintln!("lzma-bench: {}: {e}", path.display());
        return;
    }
    println!();
    println!("{}: runs", path.display());
    let (mut n, mut min, mut max) = (0u64, u64::MAX, 0u64);
    while let Some(r) = scanner.next_run() {
        if n < 8 {
            println!(
                "  #{n:<3} in {:>12} +{:<11} out {:>12} +{:<11} dict reset {}",
                r.in_offset, r.packed_len, r.out_offset, r.unpacked_len, r.has_dict_reset
            );
        }
        n += 1;
        min = min.min(r.unpacked_len);
        max = max.max(r.unpacked_len);
    }
    if n > 8 {
        println!("  ... {} more", n - 8);
    }
    println!("  {n} runs, unpacked {} to {}", human(min), human(max));
}

/// `1,2,4,all` or `sweep`.
fn parse_threads(spec: &str) -> Vec<usize> {
    let all = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let spec = if spec == "sweep" {
        "1,2,4,8,16,all"
    } else {
        spec
    };
    let mut v: Vec<usize> = spec
        .split(',')
        .map(|t| match t.trim() {
            "all" => all,
            other => other
                .parse()
                .unwrap_or_else(|_| fail("--threads takes numbers or \"all\"")),
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn fail(msg: &str) -> ! {
    eprintln!("lzma-bench: {msg}");
    std::process::exit(2);
}

struct Run {
    decode: Duration,
    total: Duration,
    bytes: u64,
    crc: u32,
}

fn bench_one(path: &Path, runs: usize, oracles: bool, portable: bool) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };

    // The decoders are interleaved, one run of each per round, rather than
    // run in blocks. On a machine that is not idle - and this one rarely is -
    // a block schedule charges whichever decoder happened to run during a
    // busy stretch, and the ratio the acceptance gate is stated in is exactly
    // the quantity that distortion moves. Interleaving spreads any load over
    // all of them, and the median then compares like with like.
    let cmds = if oracles {
        oracle_commands(path)
    } else {
        Vec::new()
    };
    let mut ours: Vec<Run> = Vec::new();
    let mut oracle_times: Vec<Option<Vec<Duration>>> =
        cmds.iter().map(|_| Some(Vec::new())).collect();

    for _ in 0..runs {
        match decode_file(path, &data, portable) {
            Ok(r) => ours.push(r),
            Err(e) => {
                eprintln!("lzma-bench: {}: {e}", path.display());
                return;
            }
        }
        for (i, (_, cmd)) in cmds.iter().enumerate() {
            let Some(slot) = oracle_times[i].as_mut() else {
                continue;
            };
            match time_command(cmd) {
                Some(t) => slot.push(t),
                None => oracle_times[i] = None,
            }
        }
    }

    let bytes = ours[0].bytes;
    let crc = ours[0].crc;
    for r in &ours {
        assert_eq!(r.bytes, bytes, "decode is not deterministic in length");
        assert_eq!(r.crc, crc, "decode is not deterministic in content");
    }

    let ours_decode = median(ours.iter().map(|r| r.decode).collect());
    let ours_total = median(ours.iter().map(|r| r.total).collect());

    println!();
    println!(
        "{}  ({} packed -> {} decoded, crc32 {crc:08x})",
        path.display(),
        human(data.len() as u64),
        human(bytes)
    );
    println!("  {:<28} {:>9} {:>11}", "decoder", "time", "MiB/s");
    print_row("lzma-turbo (decode only)", ours_decode, bytes);
    print_row("lzma-turbo (incl. crc32)", ours_total, bytes);

    if !oracles {
        return;
    }

    let mut baseline: Option<Duration> = None;
    let mut c_baseline: Option<Duration> = None;
    for (i, (label, _)) in cmds.iter().enumerate() {
        let Some(times) = oracle_times[i].take() else {
            println!("  {label:<28} {:>9}", "n/a");
            continue;
        };
        let t = median(times);
        if label.starts_with("7zz") {
            baseline = Some(t);
        }
        if label.starts_with("7lzma") {
            c_baseline = Some(t);
        }
        print_row(label, t, bytes);
    }

    if let Some(b) = c_baseline {
        let r = ours_decode.as_secs_f64() / b.as_secs_f64();
        println!("  vs 7lzma (C, no asm):  {r:.4}");
    }
    if let Some(b) = baseline {
        let r = ours_decode.as_secs_f64() / b.as_secs_f64();
        println!(
            "  gate vs 7zz -mmt=1:    {r:.4}  ({})",
            if r <= 1.03 { "within 3%" } else { "OVER" }
        );
    }
}

fn print_row(label: &str, t: Duration, bytes: u64) {
    let secs = t.as_secs_f64();
    let mibs = (bytes as f64 / (1024.0 * 1024.0)) / secs;
    println!("  {label:<28} {secs:>8.3}s {mibs:>10.1}");
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

fn human(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut x = n as f64;
    let mut u = 0;
    while x >= 1024.0 && u + 1 < UNITS.len() {
        x /= 1024.0;
        u += 1;
    }
    format!("{x:.1} {}", UNITS[u])
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn decode_file(path: &Path, data: &[u8], portable: bool) -> Result<Run, String> {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "lzma" => decode_lzma1(data, portable),
        "xz" | "7z" => {
            let (prop, payload) = lzma2_payload(path, data)?;
            decode_lzma2(prop, payload, portable)
        }
        ext => Err(format!("unsupported input extension {ext:?}")),
    }
}

fn decode_lzma1(data: &[u8], portable: bool) -> Result<Run, String> {
    if data.len() < 13 {
        return Err("truncated .lzma header".into());
    }
    let header: [u8; 13] = data[..13].try_into().unwrap();
    let header = LzmaAloneHeader::parse(&header).map_err(|e| e.to_string())?;
    let mut dec = if portable {
        LzmaDecoder::new_portable(header.props)
    } else {
        LzmaDecoder::new(header.props)
    }
    .map_err(|e| e.to_string())?;
    drive(&data[13..], &mut crc_sink(), |input, out| {
        dec.decode(input, out, FinishMode::Any)
            .map_err(|e| e.to_string())
    })
}

/// The raw LZMA2 stream of an `.xz` or `.7z` input, with its dictionary
/// property byte.
///
/// For `.7z` the property byte comes out of the archive's own coder
/// properties rather than out of `7zz l -slt`'s method string: the header
/// carries the byte the encoder wrote, whereas the method string is a
/// rendered dictionary size that has to be mapped back, and the two
/// disagree for odd-mantissa sizes (prop 26 prints as "LZMA2:25").
fn lzma2_payload<'a>(path: &Path, data: &'a [u8]) -> Result<(u8, &'a [u8]), String> {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "7z" => {
            let s = sevenz::pack_stream(data).ok_or("not a single-folder LZMA2 .7z")?;
            let end = s.offset + s.packed_len;
            if end > data.len() as u64 {
                return Err("7z pack stream runs past the end of the file".into());
            }
            Ok((s.dict_prop, &data[s.offset as usize..end as usize]))
        }
        _ => xz_lzma2_block(data).ok_or_else(|| "not a single-block LZMA2 .xz".into()),
    }
}

fn decode_lzma2(dict_prop: u8, payload: &[u8], portable: bool) -> Result<Run, String> {
    let mut dec = if portable {
        Lzma2Decoder::new_portable(dict_prop)
    } else {
        Lzma2Decoder::new(dict_prop)
    }
    .map_err(|e| e.to_string())?;
    drive(payload, &mut crc_sink(), |input, out| {
        dec.decode(input, out, FinishMode::Any)
            .map_err(|e| e.to_string())
    })
}

fn crc_sink() -> Crc32 {
    Crc32::new()
}

/// Runs the streaming loop, timing only the decoder calls so the CRC of the
/// output does not get charged to the decoder.
fn drive<F>(mut input: &[u8], crc: &mut Crc32, mut step: F) -> Result<Run, String>
where
    F: FnMut(&[u8], &mut [u8]) -> Result<lzma_turbo::Progress, String>,
{
    let mut out = vec![0u8; OUT_CHUNK];
    let mut bytes = 0u64;
    let mut decode = Duration::ZERO;
    let started = Instant::now();

    loop {
        let t0 = Instant::now();
        let p = step(input, &mut out)?;
        decode += t0.elapsed();

        input = &input[p.read..];
        crc.update(&out[..p.written]);
        bytes += p.written as u64;

        if p.status == Status::FinishedWithMark || (p.read == 0 && p.written == 0) {
            break;
        }
    }

    Ok(Run {
        decode,
        total: started.elapsed(),
        bytes,
        crc: crc.finish(),
    })
}

/// Minimal single-block `.xz` reader; see the module docs for why it is this
/// small. Mirrors the test helper of the same name.
fn xz_lzma2_block(data: &[u8]) -> Option<(u8, &[u8])> {
    const MAGIC: &[u8] = &[0xFD, b'7', b'z', b'X', b'Z', 0x00];
    if data.len() < 12 || &data[..6] != MAGIC {
        return None;
    }
    let mut pos = 12;
    let first = *data.get(pos)?;
    if first == 0 {
        return None;
    }
    let header_end = pos + (usize::from(first) + 1) * 4;
    if header_end > data.len() {
        return None;
    }
    pos += 1;
    let flags = *data.get(pos)?;
    pos += 1;
    if flags & 0x3C != 0 || flags & 0x03 != 0 {
        return None;
    }
    if flags & 0x40 != 0 {
        xz_varint(data, &mut pos)?;
    }
    if flags & 0x80 != 0 {
        xz_varint(data, &mut pos)?;
    }
    let id = xz_varint(data, &mut pos)?;
    let props_size = xz_varint(data, &mut pos)?;
    if id != 0x21 || props_size != 1 {
        return None;
    }
    let dict_prop = *data.get(pos)?;
    Some((dict_prop, &data[header_end..]))
}

fn xz_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7F) << (i * 7);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Oracles
// ---------------------------------------------------------------------------

fn oracle_commands(path: &Path) -> Vec<(String, Vec<String>)> {
    let p = path.display().to_string();
    let mut v = vec![(
        "7zz t -mmt=1 (asm loop)".to_string(),
        vec!["7zz".into(), "t".into(), "-mmt=1".into(), p.clone()],
    )];
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "lzma" => {
            v.push((
                "xz -dc -T1 --format=lzma".to_string(),
                vec![
                    "xz".into(),
                    "-dc".into(),
                    "-T1".into(),
                    "--format=lzma".into(),
                    p.clone(),
                ],
            ));
            if let Some(c) = reference_c_decoder() {
                v.push((
                    "7lzma d (C, no asm)".to_string(),
                    vec![c, "d".into(), p, "/dev/null".into()],
                ));
            }
        }
        "xz" => v.push((
            "xz -dc -T1".to_string(),
            vec!["xz".into(), "-dc".into(), "-T1".into(), p],
        )),
        _ => {}
    }
    v
}

/// The reference C decoder, `7lzma` built from `C/Util/Lzma` in the 7-Zip
/// source tree: the binary `LZMA_TURBO_7LZMA` names, or else whatever `7lzma`
/// the PATH offers.
fn reference_c_decoder() -> Option<String> {
    if let Ok(p) = std::env::var("LZMA_TURBO_7LZMA")
        && Path::new(&p).is_file()
    {
        return Some(p);
    }
    let path = std::env::var("PATH").ok()?;
    std::env::split_paths(&path)
        .map(|d| d.join("7lzma"))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

fn time_command(cmd: &[String]) -> Option<Duration> {
    let t0 = Instant::now();
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    let dt = t0.elapsed();
    status.success().then_some(dt)
}

// ---------------------------------------------------------------------------
// CRC32 (IEEE)
// ---------------------------------------------------------------------------

/// The output checksum, from the crate's own `crc` module, which is
/// `crc-fast` over the carry-less multiply units.
///
/// The single-threaded lane keeps this outside the timed region, but the
/// multi-threaded one cannot: a push decoder hands its output to a sink, and
/// the sink is where the bytes are. On the parallel path that sink runs
/// inside the ring's serialised write section, so a slow checksum is charged
/// to the decoder *and* serialises it. A table-driven CRC32 here cost 0.37 s
/// of serial time on a gigabyte - 15% of the whole decode - which is about
/// what the gap against `7zz t` was before it was replaced. `7zz t` checksums
/// its output too, with its own fast implementation; this is what makes the
/// comparison a comparison of decoders.
struct Crc32(Option<lzma_turbo::crc::Crc32>);

impl Crc32 {
    fn new() -> Self {
        Crc32(Some(lzma_turbo::crc::Crc32::new()))
    }

    fn update(&mut self, buf: &[u8]) {
        if let Some(c) = self.0.as_mut() {
            c.update(buf);
        }
    }

    fn finish(&mut self) -> u32 {
        self.0.take().map_or(0, lzma_turbo::crc::Crc32::finalize)
    }
}

// ---------------------------------------------------------------------------
// Multi-threaded LZMA2 lane
// ---------------------------------------------------------------------------

/// One timed multi-threaded decode: wall time, and the high-water mark of
/// bytes this process had allocated while it ran.
struct MtRun {
    time: Duration,
    peak: u64,
    bytes: u64,
    crc: u32,
}

fn bench_mt(path: &Path, runs: usize, oracles: bool, threads: &[usize], checksum: Checksum) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };
    let (dict_prop, payload) = match lzma2_payload(path, &data) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };
    // `7zz t -mmt=N` only understands the container, so the oracle is only
    // available for inputs 7-Zip reads.
    let sevenz_oracle = oracles && path.extension().and_then(|s| s.to_str()) == Some("7z");

    println!();
    println!(
        "{}  ({} packed, dict prop {dict_prop} = {})",
        path.display(),
        human(data.len() as u64),
        human(u64::from(dict_size_from_prop(dict_prop)))
    );
    println!(
        "  {:>7} {:>9} {:>10} {:>10} {:>9} {:>9} {:>9}",
        "threads", "ours", "MiB/s", "peak RAM", "7zz", "rust2", "ratios"
    );

    let mut first: Option<(u64, u32)> = None;
    for &t in threads {
        let mut ours: Vec<MtRun> = Vec::new();
        let mut rust2: Option<Vec<Duration>> = Some(Vec::new());
        let mut rust2_peak = 0u64;
        let mut sevenz: Option<Vec<Duration>> = sevenz_oracle.then(Vec::new);

        for _ in 0..runs {
            match mt_decode(dict_prop, payload, t, checksum) {
                Ok(r) => ours.push(r),
                Err(e) => {
                    eprintln!("lzma-bench: {} at {t} threads: {e}", path.display());
                    return;
                }
            }
            if oracles && let Some(slot) = rust2.as_mut() {
                match rust2_decode(dict_prop, payload, t) {
                    Ok(r) => {
                        rust2_peak = rust2_peak.max(r.peak);
                        slot.push(r.time);
                    }
                    Err(e) => {
                        eprintln!("lzma-bench: lzma-rust2 at {t} threads: {e}");
                        rust2 = None;
                    }
                }
            } else {
                rust2 = None;
            }
            if let Some(slot) = sevenz.as_mut() {
                let cmd = [
                    "7zz".to_string(),
                    "t".into(),
                    format!("-mmt={t}"),
                    path.display().to_string(),
                ];
                match time_command(&cmd) {
                    Some(d) => slot.push(d),
                    None => sevenz = None,
                }
            }
        }

        let bytes = ours[0].bytes;
        let crc = ours[0].crc;
        for r in &ours {
            assert_eq!(
                (r.bytes, r.crc),
                (bytes, crc),
                "MT decode is not deterministic"
            );
        }
        match first {
            Some(prev) => assert_eq!(prev, (bytes, crc), "thread count changed the output"),
            None => first = Some((bytes, crc)),
        }

        let t_ours = median(ours.iter().map(|r| r.time).collect());
        let peak = ours.iter().map(|r| r.peak).max().unwrap_or(0);
        let t_7zz = sevenz.map(median);
        let t_r2 = rust2.map(median);

        let secs = t_ours.as_secs_f64();
        let mibs = (bytes as f64 / (1024.0 * 1024.0)) / secs;
        let mut ratios = String::new();
        if let Some(b) = t_7zz {
            let r = secs / b.as_secs_f64();
            ratios.push_str(&format!("7zz {r:.3}"));
        }
        if let Some(b) = t_r2 {
            let r = secs / b.as_secs_f64();
            if !ratios.is_empty() {
                ratios.push_str(", ");
            }
            ratios.push_str(&format!("rust2 {r:.3}"));
        }
        println!(
            "  {t:>7} {secs:>8.3}s {mibs:>10.1} {:>10} {:>9} {:>9}  {ratios}",
            human(peak),
            t_7zz.map_or("n/a".to_string(), |d| format!("{:.3}s", d.as_secs_f64())),
            t_r2.map_or("n/a".to_string(), |d| format!("{:.3}s", d.as_secs_f64())),
        );
        if rust2_peak != 0 {
            println!("  {:>7} lzma-rust2 peak RAM {}", "", human(rust2_peak));
        }
    }
    println!("  (ratios are ours/oracle; below 1.000 means lzma-turbo is faster)");
}

/// One segment per 16 MiB, so a checksummed run is also a segmented one: the
/// row is meant to answer "what does asking for per-file CRCs cost?", and a
/// single segment per 128 MiB block would not.
const SPLIT_STRIDE: u64 = 16 << 20;

fn mt_decode(
    dict_prop: u8,
    payload: &[u8],
    threads: usize,
    checksum: Checksum,
) -> Result<MtRun, String> {
    let opts = Lzma2MtOptions {
        threads,
        memory_limit: u64::MAX,
    };
    let dec = Lzma2ParallelDecoder::new(dict_prop, &opts).map_err(|e| e.to_string())?;
    let mut sink = CrcWriter::new();
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let bytes = if checksum == Checksum::None {
        dec.decode(payload, &mut sink).map_err(|e| e.to_string())?
    } else {
        let plan = ChecksumPlan::new(checksum)
            .with_split_points((1..).map(|i| i * SPLIT_STRIDE).take_while(|&p| p < 1 << 31));
        let (n, checks) = dec
            .decode_checksummed(payload, &mut sink, &plan)
            .map_err(|e| e.to_string())?;
        // Fold on the caller's thread, as a consumer would, so the cost of
        // that is in the measurement too.
        let mut segs = 0usize;
        for c in &checks {
            segs += c.segments.len();
        }
        let _ = segs;
        n
    };
    let time = t0.elapsed();
    Ok(MtRun {
        time,
        peak: alloc_watch_peak(base),
        bytes,
        crc: sink.crc.finish(),
    })
}

fn rust2_decode(dict_prop: u8, payload: &[u8], threads: usize) -> Result<MtRun, String> {
    let dict = dict_size_from_prop(dict_prop);
    let mut sink = CrcWriter::new();
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut r = lzma_rust2::Lzma2ReaderMt::new(payload, dict, None, threads as u32);
    let mut buf = vec![0u8; OUT_CHUNK];
    let mut bytes = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        sink.crc.update(&buf[..n]);
    }
    let time = t0.elapsed();
    Ok(MtRun {
        time,
        peak: alloc_watch_peak(base),
        bytes,
        crc: sink.crc.finish(),
    })
}

/// C: `LZMA2_DIC_SIZE_FROM_PROP_FULL`.
fn dict_size_from_prop(p: u8) -> u32 {
    if p == 40 {
        u32::MAX
    } else {
        (2 | (u32::from(p) & 1)) << (u32::from(p) / 2 + 11)
    }
}

/// CRC-32 behind a [`Write`], so the MT decoders can be driven through their
/// real sinks. The hashing is inside the timed region here - unlike the
/// single-threaded lane, which separates them - because a push decoder has no
/// way to hand its output over without someone consuming it.
struct CrcWriter {
    crc: Crc32,
    written: u64,
}

impl CrcWriter {
    fn new() -> Self {
        CrcWriter {
            crc: Crc32::new(),
            written: 0,
        }
    }
}

impl Write for CrcWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.crc.update(buf);
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Allocation high-water mark
// ---------------------------------------------------------------------------

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Wraps the system allocator and tracks live bytes. The counters are
/// relaxed: they are a high-water mark for a report, not a synchronisation
/// mechanism, and the input file is already resident before any of them is
/// read, so what the decoders add is what shows up.
struct Counting;

#[global_allocator]
static ALLOC: Counting = Counting;

fn note_alloc(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            note_alloc(l.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            note_alloc(l.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) };
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                note_alloc(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

/// Starts a measurement: returns the live-bytes baseline and pulls the peak
/// down to it.
fn alloc_watch_reset() -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    base
}

fn alloc_watch_peak(base: usize) -> u64 {
    PEAK.load(Ordering::Relaxed).saturating_sub(base) as u64
}

/// Times the adaptive decoder on a stream that is already on disk, against the
/// ring on the same stream.
///
/// This is the consumer's shape, not a download's: the whole stream is
/// available, fed in large pieces, and drained as it goes. What it measures is
/// how much of the work ends up on the calling thread - the chase decoder
/// serialises everything while it holds the cursor, so a decoder that chases
/// when it could have waited shows up here as a flat curve.
fn bench_adaptive(path: &Path, runs: usize, threads: &[usize]) {
    const FEED: usize = 1 << 24;

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };
    let (dict_prop, payload) = match lzma2_payload(path, &data) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };
    let threads: Vec<usize> = if threads.is_empty() {
        vec![1, 2, 4, 8]
    } else {
        threads.to_vec()
    };

    println!();
    println!(
        "{}  ({} packed, adaptive vs ring)",
        path.display(),
        human(data.len() as u64)
    );
    println!(
        "  {:>7} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "threads", "chasing", "waiting", "MiB/s", "ring", "wait/ring"
    );

    let mut first: Option<(u64, u32)> = None;
    for &t in &threads {
        let mut ad: Vec<MtRun> = Vec::new();
        let mut wait: Vec<Duration> = Vec::new();
        let mut ring: Vec<Duration> = Vec::new();
        for _ in 0..runs {
            match adaptive_decode(dict_prop, payload, t, FEED, true) {
                Ok(r) => ad.push(r),
                Err(e) => {
                    eprintln!("lzma-bench: adaptive at {t} threads: {e}");
                    return;
                }
            }
            match adaptive_decode(dict_prop, payload, t, FEED, false) {
                Ok(r) => wait.push(r.time),
                Err(e) => {
                    eprintln!("lzma-bench: adaptive (waiting) at {t} threads: {e}");
                    return;
                }
            }
            match mt_decode(dict_prop, payload, t, Checksum::None) {
                Ok(r) => ring.push(r.time),
                Err(e) => {
                    eprintln!("lzma-bench: ring at {t} threads: {e}");
                    return;
                }
            }
        }
        let bytes = ad[0].bytes;
        let crc = ad[0].crc;
        match first {
            Some(prev) => assert_eq!(prev, (bytes, crc), "thread count changed the output"),
            None => first = Some((bytes, crc)),
        }
        let t_ad = median(ad.iter().map(|r| r.time).collect());
        let t_wait = median(wait);
        let t_ring = median(ring);
        let secs = t_wait.as_secs_f64();
        println!(
            "  {:>7} {:>9.3}s {:>9.3}s {:>10.1} {:>9.3}s {:>8.3}",
            t,
            t_ad.as_secs_f64(),
            secs,
            bytes as f64 / (1024.0 * 1024.0) / secs,
            t_ring.as_secs_f64(),
            secs / t_ring.as_secs_f64()
        );
    }
}

/// One adaptive decode of an on-disk stream: feed, drain, repeat.
fn adaptive_decode(
    dict_prop: u8,
    payload: &[u8],
    threads: usize,
    feed: usize,
    chase: bool,
) -> Result<MtRun, String> {
    let opts = Lzma2MtOptions {
        threads,
        memory_limit: u64::MAX,
    };
    let mut dec = Lzma2AdaptiveDecoder::new(dict_prop, &opts).map_err(|e| e.to_string())?;
    dec.set_chase(chase);
    let mut crc = lzma_turbo::crc::Crc32::new();
    let mut total = 0u64;
    let base = alloc_watch_reset();
    let t0 = Instant::now();
    let mut pos = 0usize;
    loop {
        if pos < payload.len() {
            let end = (pos + feed).min(payload.len());
            pos += dec.feed(&payload[pos..end]).map_err(|e| e.to_string())?;
            if pos == payload.len() {
                dec.end_of_input();
            }
        }
        let status = dec
            .drain(|_, b| {
                total += b.len() as u64;
                crc.update(b);
            })
            .map_err(|e| e.to_string())?;
        if status == DrainStatus::Finished {
            break;
        }
    }
    let time = t0.elapsed();
    Ok(MtRun {
        time,
        peak: alloc_watch_peak(base),
        bytes: total,
        crc: crc.finalize(),
    })
}
