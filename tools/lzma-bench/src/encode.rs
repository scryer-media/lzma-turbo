//! The encode lane: this crate's `.xz` writer against `xz` at matching
//! presets. See `docs/benchmarking.md`.
//!
//! The question an encoder bench answers is two-dimensional — a compressor is
//! only faster than another at the same ratio — so every row carries both the
//! wall time and the bytes produced. Presets line up because the port is
//! bit-exact with the SDK's encoder and `xz -N` maps its own presets onto the
//! same `lc`/`lp`/`pb`/dictionary/fast-bytes settings; where the two differ,
//! the size column says so.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lzma_turbo::xz::CheckType;
use lzma_turbo::{LzmaEncProps, auto_block_size, encode_xz, encode_xz_mt};

use crate::{human, median};

/// One timed compression.
struct Row {
    label: String,
    time: Duration,
    out: u64,
}

/// Times `xz -T<threads> -<preset> -c` over `data`, returning the elapsed time
/// and the size it produced, or `None` if `xz` is not there.
fn time_xz(preset: u32, threads: usize, data: &[u8]) -> Option<(Duration, u64)> {
    let arg = format!("-{preset}");
    let t_arg = format!("-T{threads}");
    let t0 = Instant::now();
    let mut child = Command::new("xz")
        .args([&t_arg, "-c", "-k", &arg])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let owned = data.to_vec();
    // The writer runs on its own thread: `xz` will not drain stdin while its
    // stdout pipe is full, so a parent that writes it all first deadlocks.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let out = child.wait_with_output().ok()?;
    writer.join().ok()?;
    let dt = t0.elapsed();
    out.status
        .success()
        .then_some((dt, out.stdout.len() as u64))
}

/// The crate's settings for `xz -N`. `with_level(N)` is 7-Zip's level N,
/// whose dictionary and match-finder depth differ from liblzma's preset N
/// (level 1 is a 256 KiB dictionary where `xz -1` uses 1 MiB), so a row
/// built on it compares two different encodes. `xz_preset` is liblzma's own
/// table, which is what the oracle column runs.
fn props_for(preset: u32) -> LzmaEncProps {
    LzmaEncProps::xz_preset(preset, false)
        .unwrap_or_else(|e| crate::fail(&format!("preset {preset}: {e}")))
}

/// Compresses `data` once and returns how long it took and how big it came
/// out.
///
/// At one thread this is the solid single-block writer, exactly as before
/// block threads existed. Above one it splits at the block size `xz -T` would
/// use, which is what makes the two comparable: the ratio cost of splitting is
/// the same on both sides.
fn time_ours(preset: u32, threads: usize, mf_threads: u32, data: &[u8]) -> (Duration, u64) {
    let props = props_for(preset).with_num_threads(mf_threads);
    let t0 = Instant::now();
    let out = if threads <= 1 {
        encode_xz(data, &props, CheckType::Crc64, 0).expect("encode")
    } else {
        let block = auto_block_size(props.normalized().dict_size);
        encode_xz_mt(data, &props, CheckType::Crc64, block, &[], threads).expect("encode")
    };
    (t0.elapsed(), out.len() as u64)
}

/// Runs the lane over one input at each preset in `presets`.
pub fn bench(
    path: &Path,
    runs: usize,
    oracles: bool,
    presets: &[u32],
    threads: &[usize],
    mf_threads: u32,
) {
    let threads: Vec<usize> = if threads.is_empty() {
        vec![1]
    } else {
        threads.to_vec()
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lzma-bench: {}: {e}", path.display());
            return;
        }
    };

    println!();
    println!(
        "{}  ({} to compress)",
        path.display(),
        human(data.len() as u64)
    );
    println!(
        "  {:<28} {:>9} {:>11} {:>13} {:>8}",
        "encoder", "time", "MiB/s", "output", "ratio"
    );

    for &preset in presets {
        for &t in &threads {
            // Interleaved, one run of each per round, for the reason the decode
            // lane gives: a block schedule charges whichever encoder happened to
            // run during a busy stretch.
            let mut ours: Vec<Duration> = Vec::new();
            let mut theirs: Vec<Duration> = Vec::new();
            let mut ours_size = 0u64;
            let mut xz_size: Option<u64> = None;
            let mut xz_gone = !oracles;

            for _ in 0..runs {
                let (dt, n) = time_ours(preset, t, mf_threads, &data);
                ours.push(dt);
                ours_size = n;
                if !xz_gone {
                    match time_xz(preset, t, &data) {
                        Some((t, n)) => {
                            theirs.push(t);
                            xz_size = Some(n);
                        }
                        None => xz_gone = true,
                    }
                }
            }

            let mf = if mf_threads > 1 {
                format!(" mf{mf_threads}")
            } else {
                String::new()
            };
            let mut rows = vec![Row {
                label: format!("lzma-turbo -{preset} -T{t}{mf}"),
                time: median(ours),
                out: ours_size,
            }];
            if let Some(n) = xz_size {
                rows.push(Row {
                    label: format!("xz -T{t} -{preset}"),
                    time: median(theirs),
                    out: n,
                });
            }

            let base = rows[0].time.as_secs_f64();
            for r in &rows {
                let secs = r.time.as_secs_f64();
                let mibs = (data.len() as f64 / (1024.0 * 1024.0)) / secs;
                let ratio = secs / base;
                println!(
                    "  {:<28} {secs:>8.3}s {mibs:>10.2} {:>13} {ratio:>7.3}x",
                    r.label,
                    human(r.out)
                );
            }
            if let Some(n) = xz_size {
                let d = ours_size as f64 / n as f64;
                println!("      size vs xz: {d:.4}x ({} vs {})", ours_size, n);
            } else if oracles {
                println!("      xz not on PATH; ours only");
            }
        }
    }
}

/// `1,5,9`, or `sweep` for the presets the acceptance gate quotes.
pub fn parse_presets(spec: &str) -> Vec<u32> {
    let spec = if spec == "sweep" { "1,3,5,6,9" } else { spec };
    let mut v: Vec<u32> = spec
        .split(',')
        .map(|t| {
            t.trim()
                .parse()
                .unwrap_or_else(|_| crate::fail("--encode takes preset numbers or \"sweep\""))
        })
        .collect();
    v.retain(|p| *p <= 9);
    v.sort_unstable();
    v.dedup();
    if v.is_empty() { vec![6] } else { v }
}
