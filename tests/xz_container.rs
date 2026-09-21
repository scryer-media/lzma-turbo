//! The `.xz` container, against `xz(1)` and against committed vectors.
//!
//! Every case here is differential: the bytes come out of `xz` (or out of a
//! vector `xz` produced), and what this crate decodes has to equal what went
//! in. The cases that need the `xz` binary skip themselves when it is absent,
//! so the suite still runs on a machine without it; the committed vectors do
//! not need it and cover the common shapes.
//!
//! The container lives behind the `xz` feature, so this whole file does with
//! it.
#![cfg(feature = "xz")]

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use lzma_turbo::DrainStatus;
use lzma_turbo::xz::{XzAdaptiveDecoder, XzOptions, XzParallelReader, XzReader};
use std::io::Cursor;

/// Compresses `data` with the `xz` binary, or `None` if it is not installed.
///
/// The input goes through a temporary file rather than a pipe: `xz` writes its
/// output as it reads, so feeding a large payload down a pipe while nothing
/// drains stdout deadlocks both processes.
fn xz_compress(args: &[&str], data: &[u8]) -> Option<Vec<u8>> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "lzma-turbo-xz-{}-{}.bin",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, data).expect("temp input");
    let out = Command::new("xz")
        .args(args)
        .arg("-c")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok();
    let _ = std::fs::remove_file(&path);
    let out = out?;
    assert!(out.status.success(), "xz {args:?} failed");
    Some(out.stdout)
}

/// Decodes with the default options, in one go.
fn decode(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    XzReader::new(data).read_to_end(&mut out)?;
    Ok(out)
}

/// Decodes in `chunk`-sized reads, which is what exercises the state machine's
/// ability to stop anywhere.
fn decode_chunked(data: &[u8], chunk: usize) -> std::io::Result<Vec<u8>> {
    let mut r = XzReader::new(data);
    let mut out = Vec::new();
    let mut buf = vec![0u8; chunk];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// A few kinds of input, small enough to compress quickly and varied enough
/// that the filters have something to do.
fn payloads() -> Vec<(&'static str, Vec<u8>)> {
    let text: Vec<u8> = std::iter::repeat_n(
        b"the quick brown fox jumps over the lazy dog\n".as_slice(),
        2000,
    )
    .flatten()
    .copied()
    .collect();
    let rand = common::pseudo_random(300_000, 0x5eed);
    let mut mixed = text.clone();
    mixed.extend_from_slice(&rand[..50_000]);
    vec![
        ("empty", Vec::new()),
        ("tiny", b"x".to_vec()),
        ("text", text),
        ("rand", rand),
        ("mixed", mixed),
    ]
}

#[test]
fn committed_vectors_decode_to_their_sources() {
    for source in ["text", "rand", "zeros", "mixed", "tiny", "empty"] {
        for variant in ["p1", "lc1lp1pb0"] {
            let name = format!("{source}.{variant}.xz");
            let path = common::data_dir().join(&name);
            if !path.exists() {
                continue;
            }
            let want = common::read(&format!("src_{source}.bin"));
            let have = decode(&std::fs::read(&path).expect("read")).expect(&name);
            assert_eq!(have, want, "{name}");
            for chunk in [1usize, 3, 1000, 65_536] {
                let have = decode_chunked(&std::fs::read(&path).expect("read"), chunk)
                    .unwrap_or_else(|e| panic!("{name} at chunk {chunk}: {e}"));
                assert_eq!(have, want, "{name} at chunk {chunk}");
            }
        }
    }
}

#[test]
fn the_committed_container_shapes_decode_the_same_three_ways() {
    // One source, six container shapes and a concatenated pair: the block
    // layout, the filters and the checks, without needing `xz` installed.
    let cases: &[(&str, &str)] = &[
        ("mixed.mb.xz", "src_mixed.bin"),
        ("mixed.bcjx86.xz", "src_mixed.bin"),
        ("mixed.delta4.xz", "src_mixed.bin"),
        ("mixed.sha256.xz", "src_mixed.bin"),
        ("mixed.crc32.xz", "src_mixed.bin"),
        ("mixed.nocheck.xz", "src_mixed.bin"),
    ];
    for (name, source) in cases {
        let path = common::data_dir().join(name);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read");
        let want = common::read(source);
        assert_eq!(decode(&bytes).expect(name), want, "{name} sequential");
        for chunk in [1usize, 7, 4096] {
            assert_eq!(
                decode_chunked(&bytes, chunk).expect(name),
                want,
                "{name} in {chunk}-byte reads"
            );
        }
        for threads in [1usize, 4] {
            assert_eq!(
                decode_parallel(&bytes, threads, 4096).expect(name),
                want,
                "{name} on {threads} threads"
            );
            assert_eq!(
                decode_adaptive(&bytes, threads, 13).expect(name),
                want,
                "{name} adaptive on {threads} threads"
            );
        }
    }

    // And the concatenated pair, which has to decode to its source twice.
    let path = common::data_dir().join("tiny.concat.xz");
    if path.exists() {
        let bytes = std::fs::read(&path).expect("read");
        let one = common::read("src_tiny.bin");
        let mut want = one.clone();
        want.extend_from_slice(&one);
        assert_eq!(decode(&bytes).expect("concat"), want);
        assert_eq!(decode_parallel(&bytes, 2, 5).expect("concat"), want);
        assert_eq!(decode_adaptive(&bytes, 2, 5).expect("concat"), want);
    }
}

#[test]
fn every_preset_check_and_filter_round_trips() {
    let cases: &[&[&str]] = &[
        &["-0"],
        &["-6"],
        &["-9", "-e"],
        &["--check=none"],
        &["--check=crc32"],
        &["--check=crc64"],
        &["--check=sha256"],
        &["--delta=dist=1", "--lzma2=preset=6"],
        &["--delta=dist=4", "--lzma2=preset=1"],
        &["--x86", "--lzma2=preset=6"],
        &["--arm", "--lzma2=preset=1"],
        &["--armthumb", "--lzma2=preset=1"],
        &["--arm64", "--lzma2=preset=1"],
        &["--powerpc", "--lzma2=preset=1"],
        &["--sparc", "--lzma2=preset=1"],
        &["--ia64", "--lzma2=preset=1"],
        &["--riscv", "--lzma2=preset=1"],
        &["--x86", "--delta=dist=2", "--lzma2=preset=1"],
        &["--block-size=4096", "-1"],
        &["-T4", "--block-size=8192", "-1"],
    ];
    for (name, data) in payloads() {
        for args in cases {
            let Some(stream) = xz_compress(args, &data) else {
                eprintln!("xz(1) not installed; skipping");
                return;
            };
            let have = decode(&stream).unwrap_or_else(|e| panic!("{name} {args:?}: {e}"));
            assert_eq!(have, data, "{name} {args:?}");
        }
    }
}

#[test]
fn odd_read_sizes_and_filters_agree() {
    let data = payloads()
        .into_iter()
        .find(|(n, _)| *n == "mixed")
        .expect("mixed")
        .1;
    for args in [
        vec!["--x86", "--lzma2=preset=1"],
        vec!["--delta=dist=3", "--lzma2=preset=1"],
        vec!["-T4", "--block-size=8192", "-1"],
    ] {
        let Some(stream) = xz_compress(&args, &data) else {
            return;
        };
        for chunk in [1usize, 2, 7, 13, 4095, 1 << 20] {
            let have = decode_chunked(&stream, chunk)
                .unwrap_or_else(|e| panic!("{args:?} at chunk {chunk}: {e}"));
            assert_eq!(have, data, "{args:?} at chunk {chunk}");
        }
    }
}

#[test]
fn concatenated_streams_and_stream_padding() {
    let a = b"first stream contents, long enough to compress\n".repeat(50);
    let b = b"second stream contents\n".repeat(50);
    let (Some(xa), Some(xb)) = (xz_compress(&["-1"], &a), xz_compress(&["-1"], &b)) else {
        return;
    };

    let mut both = xa.clone();
    both.extend_from_slice(&xb);
    let mut want = a.clone();
    want.extend_from_slice(&b);
    assert_eq!(decode(&both).expect("concatenated"), want);

    // Stream padding: null bytes in whole four-byte groups, before the next
    // stream and at the end of the file.
    let mut padded = xa.clone();
    padded.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    padded.extend_from_slice(&xb);
    padded.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(decode(&padded).expect("padded"), want);

    // A single-stream reader must refuse what follows the first stream.
    let mut r = XzReader::new(&both[..]).single_stream();
    let mut out = Vec::new();
    let err = r.read_to_end(&mut out).expect_err("trailing data");
    assert_eq!(out, a);
    assert!(format!("{err}").contains("trailing"), "{err}");

    // Padding that is not a multiple of four is not padding.
    let mut ragged = xa.clone();
    ragged.extend_from_slice(&[0, 0]);
    assert!(decode(&ragged).is_err());
}

#[test]
fn truncation_at_every_byte_is_an_error_and_never_a_panic() {
    let data = b"a small but compressible payload\n".repeat(30);
    let Some(stream) = xz_compress(&["-1"], &data) else {
        return;
    };
    for cut in 0..stream.len() {
        let mut out = Vec::new();
        let r = XzReader::new(&stream[..cut]).read_to_end(&mut out);
        assert!(r.is_err(), "truncation at {cut} decoded cleanly");
        assert!(out.len() <= data.len());
        assert_eq!(
            out,
            data[..out.len()],
            "wrong bytes before the cut at {cut}"
        );
    }
}

#[test]
fn single_byte_corruption_is_caught() {
    let data = b"a small but compressible payload\n".repeat(30);
    let Some(stream) = xz_compress(&["-1"], &data) else {
        return;
    };
    let mut caught = 0usize;
    for i in 0..stream.len() {
        for bit in [0x01u8, 0x80] {
            let mut bad = stream.clone();
            bad[i] ^= bit;
            let mut out = Vec::new();
            match XzReader::new(&bad[..]).read_to_end(&mut out) {
                Err(_) => caught += 1,
                // A flip inside the compressed data can still decode to the
                // same bytes only if it was in a field nothing depends on;
                // the check would have caught anything else.
                Ok(_) => assert_eq!(out, data, "corruption at {i} bit {bit:#x} went unnoticed"),
            }
        }
    }
    assert!(caught > stream.len(), "hardly any corruption was caught");
}

#[test]
fn the_memory_limit_is_enforced_and_block_sizes_shrink_it() {
    let data = b"payload\n".repeat(4096);
    let Some(stream) = xz_compress(&["-9"], &data) else {
        return;
    };
    // -9 declares a 64 MiB dictionary, but the block header declares an
    // uncompressed size far below it, so a small limit is still enough.
    let opts = XzOptions::default().with_memory_limit(1 << 20);
    let mut out = Vec::new();
    XzReader::with_options(&stream[..], opts)
        .read_to_end(&mut out)
        .expect("small dictionary");
    assert_eq!(out, data);

    // A limit below even the minimum dictionary is refused rather than
    // rounded up.
    let opts = XzOptions::default().with_memory_limit(16);
    let err = XzReader::with_options(&stream[..], opts)
        .read_to_end(&mut Vec::new())
        .expect_err("limit");
    assert!(format!("{err}").contains("limit"), "{err}");
}

#[test]
fn an_output_cap_stops_a_decode() {
    let data = b"payload\n".repeat(4096);
    let Some(stream) = xz_compress(&["-1"], &data) else {
        return;
    };
    let opts = XzOptions::default().with_max_unpack_bytes(Some(100));
    let mut out = Vec::new();
    let err = XzReader::with_options(&stream[..], opts)
        .read_to_end(&mut out)
        .expect_err("cap");
    assert!(format!("{err}").contains("cap"), "{err}");
    assert!(out.len() <= 100);
}

/// A single-threaded `xz` writes no sizes into the block header, so the only
/// limit the block has is the caller's cap. Reaching it is a cap hit, not
/// corrupt data: LZMA2 asked to finish with nothing left to write into fails
/// on the chunks still to come, and that failure is the cap's.
#[test]
fn an_output_cap_stops_a_block_that_declares_no_size() {
    let data = b"payload\n".repeat(4096);
    let Some(stream) = xz_compress(&["-1", "-T1"], &data) else {
        return;
    };
    let opts = XzOptions::default().with_max_unpack_bytes(Some(100));
    let mut out = Vec::new();
    let err = XzReader::with_options(&stream[..], opts)
        .read_to_end(&mut out)
        .expect_err("cap");
    assert!(format!("{err}").contains("cap"), "{err}");
    assert!(out.len() <= 100);
}

/// Decodes with the parallel reader at `threads`, in `chunk`-sized reads.
fn decode_parallel(data: &[u8], threads: usize, chunk: usize) -> std::io::Result<Vec<u8>> {
    let opts = XzOptions::default().with_threads(threads);
    let mut r = XzParallelReader::with_options(Cursor::new(data.to_vec()), opts)?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; chunk];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

#[test]
fn parallel_matches_sequential_byte_for_byte() {
    let data = payloads()
        .into_iter()
        .find(|(n, _)| *n == "mixed")
        .expect("mixed")
        .1;
    for args in [
        vec!["-T4", "--block-size=8192", "-1"],
        vec!["--block-size=4096", "-1"],
        vec!["--x86", "--block-size=16384", "-1"],
        vec!["--delta=dist=4", "--block-size=16384", "-1"],
        vec!["--check=sha256", "--block-size=16384", "-1"],
        vec!["--check=none", "--block-size=16384", "-1"],
        vec!["-1"],
    ] {
        let Some(stream) = xz_compress(&args, &data) else {
            return;
        };
        let want = decode(&stream).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        assert_eq!(want, data, "{args:?}");
        for threads in [1usize, 2, 8, 16] {
            for chunk in [1usize, 7, 4095, 1 << 20] {
                let have = decode_parallel(&stream, threads, chunk).unwrap_or_else(|e| {
                    panic!("{args:?} at {threads} threads, chunk {chunk}: {e}")
                });
                assert_eq!(have, data, "{args:?} at {threads} threads, chunk {chunk}");
            }
        }
    }
}

#[test]
fn parallel_reads_concatenated_streams_and_padding() {
    let a = b"first stream, compressible\n".repeat(400);
    let b = b"second stream, also compressible\n".repeat(400);
    let (Some(xa), Some(xb)) = (
        xz_compress(&["--block-size=4096", "-1"], &a),
        xz_compress(&["--block-size=4096", "-1"], &b),
    ) else {
        return;
    };
    let mut both = xa.clone();
    both.extend_from_slice(&[0, 0, 0, 0]);
    both.extend_from_slice(&xb);
    both.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    let mut want = a.clone();
    want.extend_from_slice(&b);
    for threads in [1usize, 4] {
        assert_eq!(
            decode_parallel(&both, threads, 1 << 16).expect("both"),
            want
        );
    }
}

#[test]
fn the_parallel_reader_degrades_threads_to_fit_its_limit() {
    let data = common::pseudo_random(400_000, 7);
    let Some(stream) = xz_compress(&["--block-size=32768", "-1"], &data) else {
        return;
    };
    let full = XzParallelReader::with_options(
        Cursor::new(stream.clone()),
        XzOptions::default().with_threads(8),
    )
    .expect("map");
    assert!(full.block_count() > 1);
    let per_worker = full.memory_estimate() / full.threads() as u64;

    // A limit that pays for exactly two workers must produce two, not eight.
    let two = XzParallelReader::with_options(
        Cursor::new(stream.clone()),
        XzOptions::default()
            .with_threads(8)
            .with_memory_limit(per_worker * 2),
    )
    .expect("map");
    assert_eq!(two.threads(), 2);
    assert!(two.memory_estimate() <= per_worker * 2);

    // A limit below one worker is refused rather than silently exceeded.
    let refused = XzParallelReader::with_options(
        Cursor::new(stream),
        XzOptions::default().with_memory_limit(per_worker / 2),
    );
    match refused {
        Ok(_) => panic!("a limit below one worker was accepted"),
        Err(e) => assert!(
            matches!(e.kind, lzma_turbo::XzErrorKind::MemoryLimit { .. }),
            "{e}"
        ),
    }
}

#[test]
fn a_corrupt_block_fails_the_parallel_decode_without_a_panic() {
    let data = common::pseudo_random(200_000, 11);
    let Some(stream) = xz_compress(&["--block-size=16384", "-1"], &data) else {
        return;
    };
    // Flip a byte in the middle, which is inside some block's data.
    for at in [stream.len() / 3, stream.len() / 2, stream.len() - 40] {
        let mut bad = stream.clone();
        bad[at] ^= 0x40;
        let mut out = Vec::new();
        let r = XzParallelReader::with_options(Cursor::new(bad), XzOptions::default()).and_then(
            |mut r| {
                r.read_to_end(&mut out).map_err(|e| {
                    lzma_turbo::XzError::at(
                        lzma_turbo::XzErrorKind::TruncatedInput,
                        0,
                        e.raw_os_error().unwrap_or(0) as u64,
                    )
                })
            },
        );
        assert!(
            r.is_err() || out == data,
            "corruption at {at} went unnoticed"
        );
    }
}

// -- the adaptive decoder ------------------------------------------------

/// Feeds `data` to the adaptive decoder in `chunk`-sized pieces, draining
/// after each, and reassembles the output by the offsets it is handed rather
/// than by the order it is handed them in.
fn decode_adaptive(
    data: &[u8],
    threads: usize,
    chunk: usize,
) -> Result<Vec<u8>, lzma_turbo::XzError> {
    let opts = XzOptions::default().with_threads(threads);
    let mut dec = XzAdaptiveDecoder::new(opts);
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos < data.len() {
            let end = (pos + chunk).min(data.len());
            let took = dec.feed(&data[pos..end])?;
            pos += took;
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let status = dec.drain(|off, bytes| {
            let off = usize::try_from(off).expect("offset");
            if out.len() < off + bytes.len() {
                out.resize(off + bytes.len(), 0u8);
            }
            out[off..off + bytes.len()].copy_from_slice(bytes);
        })?;
        match status {
            DrainStatus::Finished => return Ok(out),
            DrainStatus::NeedsMoreInput if pos == data.len() => {
                panic!("the decoder wanted input after the file ended")
            }
            _ => {}
        }
    }
}

#[test]
fn the_adaptive_decoder_matches_the_sequential_one() {
    for (name, payload) in payloads() {
        for args in [
            vec!["-6"],
            vec!["-9", "--check=crc64"],
            vec!["--check=sha256", "--x86", "--lzma2=preset=3"],
            vec!["-1", "--check=none"],
        ] {
            let Some(data) = xz_compress(&args, &payload) else {
                return;
            };
            for threads in [1usize, 2, 8] {
                for chunk in [1usize, 13, 4096, 1 << 20] {
                    let got = decode_adaptive(&data, threads, chunk)
                        .unwrap_or_else(|e| panic!("{name} {args:?} t{threads} c{chunk}: {e}"));
                    assert_eq!(
                        got, payload,
                        "{name} {args:?} threads {threads} chunk {chunk}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_adaptive_decoder_dispatches_a_multi_block_file() {
    let payload = common::pseudo_random(4 << 20, 0xab1e);
    let Some(data) = xz_compress(&["-1", "--block-size=262144", "-T4"], &payload) else {
        return;
    };
    for threads in [1usize, 4, 16] {
        for chunk in [7usize, 65_536, 1 << 20] {
            let got = decode_adaptive(&data, threads, chunk).expect("adaptive");
            assert_eq!(got, payload, "threads {threads} chunk {chunk}");
        }
    }

    // And the blocks really did go to workers rather than all being chased.
    let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(4));
    let mut pos = 0usize;
    let mut total = 0u64;
    loop {
        if pos < data.len() {
            let end = (pos + (1 << 20)).min(data.len());
            pos += dec.feed(&data[pos..end]).expect("feed");
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let status = dec.drain(|_, b| total += b.len() as u64).expect("drain");
        if status == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(total, payload.len() as u64);
    assert!(
        dec.spawned_threads() > 1,
        "a multi-block file was decoded on one thread"
    );
}

#[test]
fn the_adaptive_decoder_handles_concatenated_streams() {
    let a = common::pseudo_random(100_000, 1);
    let b = common::pseudo_random(60_000, 2);
    let (Some(mut xa), Some(xb)) = (
        xz_compress(&["-2", "--check=crc32"], &a),
        xz_compress(&["-4", "--check=crc64"], &b),
    ) else {
        return;
    };
    xa.extend_from_slice(&[0, 0, 0, 0]);
    xa.extend_from_slice(&xb);
    let mut want = a.clone();
    want.extend_from_slice(&b);
    for chunk in [5usize, 1024, 1 << 20] {
        assert_eq!(decode_adaptive(&xa, 4, chunk).expect("adaptive"), want);
    }
}

#[test]
fn waiting_for_a_worker_replaces_the_drain_spin() {
    // A caller with every byte of the file already fed and nothing more it
    // wants to hand over: `drain` returns rather than wait, so without
    // somewhere to block such a caller would call it until a worker answered.
    let payload = common::pseudo_random(3 << 20, 0xb10c);
    let Some(data) = xz_compress(&["-1", "--block-size=262144", "-T4"], &payload) else {
        return;
    };
    let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(4));
    assert!(!dec.wait_for_worker(), "waited with nothing dispatched");

    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        pos += dec.feed(&data[pos..]).expect("feed");
    }
    let mut waited = 0usize;
    let mut turns = 0usize;
    loop {
        turns += 1;
        assert!(turns < 10_000, "the drain loop did not converge");
        let before = out.len();
        let status = dec.drain(|_, b| out.extend_from_slice(b)).expect("drain");
        if status == DrainStatus::Finished {
            break;
        }
        if out.len() != before {
            continue;
        }
        if dec.wait_for_worker() {
            waited += 1;
            continue;
        }
        dec.end_of_input();
    }
    assert_eq!(out, payload);
    assert!(!dec.wait_for_worker(), "a block outlived the finished file");
    // How often the wait is actually reached depends on how the file was
    // blocked and on which worker finishes when, so the count is reported
    // rather than asserted; what the loop proves is that waiting instead of
    // re-draining decodes the same bytes and always converges.
    assert!(waited <= turns);
}

#[test]
fn the_thread_count_can_change_mid_stream() {
    let payload = common::pseudo_random(3 << 20, 0xfeed);
    let Some(data) = xz_compress(&["-1", "--block-size=262144", "-T4"], &payload) else {
        return;
    };
    let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(1));
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut turns = 0u32;
    loop {
        if pos < data.len() {
            let end = (pos + 100_000).min(data.len());
            pos += dec.feed(&data[pos..end]).expect("feed");
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let status = dec
            .drain(|off, bytes| {
                let off = usize::try_from(off).expect("offset");
                if out.len() < off + bytes.len() {
                    out.resize(off + bytes.len(), 0u8);
                }
                out[off..off + bytes.len()].copy_from_slice(bytes);
            })
            .expect("drain");
        turns += 1;
        dec.set_threads(match turns % 3 {
            0 => 1,
            1 => 8,
            _ => 3,
        });
        if status == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(out, payload);
}

#[test]
fn a_truncated_feed_that_ends_is_an_error_not_a_hang() {
    let payload = common::pseudo_random(200_000, 9);
    let Some(data) = xz_compress(&["-2"], &payload) else {
        return;
    };
    for cut in [1usize, 12, 40, data.len() / 2, data.len() - 1] {
        let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(2));
        let mut fed = 0usize;
        while fed < cut {
            fed += dec.feed(&data[fed..cut]).expect("feed");
        }
        dec.end_of_input();
        let mut err = None;
        loop {
            match dec.drain(|_, _| {}) {
                Ok(DrainStatus::Finished) => break,
                Ok(DrainStatus::NeedsMoreInput) => {
                    panic!("wanted input after end_of_input at cut {cut}")
                }
                Ok(DrainStatus::Progress) => {}
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        assert!(err.is_some(), "truncation at {cut} was accepted");
    }
}

#[test]
fn corrupting_a_byte_fails_the_adaptive_decode() {
    let payload = common::pseudo_random(120_000, 11);
    let Some(data) = xz_compress(&["-2", "--check=crc64"], &payload) else {
        return;
    };
    for at in [8usize, 40, data.len() / 3, data.len() - 6] {
        let mut bad = data.clone();
        bad[at] ^= 0x40;
        if let Ok(got) = decode_adaptive(&bad, 2, 4096) {
            assert_ne!(got, payload, "corruption at {at} decoded to the original");
        }
    }
}

// -- the structural gates ------------------------------------------------

/// `(streams, blocks, uncompressed size)` as `xz -l --robot` reports them.
fn xz_list(path: &std::path::Path) -> Option<(u64, u64, u64)> {
    let out = Command::new("xz")
        .args(["-l", "--robot"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    // The `file` line is: name, streams, blocks, compressed, uncompressed, ...
    let line = text.lines().find(|l| l.starts_with("file\t"))?;
    let f: Vec<&str> = line.split('\t').collect();
    Some((
        f.get(1)?.parse().ok()?,
        f.get(2)?.parse().ok()?,
        f.get(4)?.parse().ok()?,
    ))
}

#[test]
fn the_structural_gates_agree_with_xz_l_robot() {
    let payload = common::pseudo_random(700_000, 0x9a7e);
    let cases: Vec<Vec<&str>> = vec![
        vec!["-1"],
        vec!["-1", "--block-size=65536"],
        vec!["-1", "--block-size=65536", "-T4"],
        vec!["-6", "--check=sha256"],
    ];
    for args in cases {
        let Some(data) = xz_compress(&args, &payload) else {
            return;
        };
        for (name, bytes) in [
            ("single", data.clone()),
            ("concatenated", {
                let mut two = data.clone();
                two.extend_from_slice(&[0, 0, 0, 0]);
                two.extend_from_slice(&data);
                two
            }),
        ] {
            let path = std::env::temp_dir()
                .join(format!("lzma-turbo-gate-{}-{name}.xz", std::process::id()));
            std::fs::write(&path, &bytes).expect("temp file");
            let listed = xz_list(&path);
            let _ = std::fs::remove_file(&path);
            let Some((streams, blocks, uncompressed)) = listed else {
                return;
            };

            // `probe` sees a stream header at byte zero either way.
            assert!(
                lzma_turbo::xz::probe(&bytes).is_some(),
                "{args:?} {name}: probe missed a stream header"
            );

            let mut cur = Cursor::new(&bytes);
            let count = lzma_turbo::xz::single_stream_block_count(&mut cur);
            if streams == 1 {
                assert_eq!(
                    count,
                    Some(usize::try_from(blocks).expect("blocks")),
                    "{args:?} {name}: block count"
                );
                assert_eq!(
                    lzma_turbo::xz::is_single_stream_multi_block(&mut cur),
                    blocks > 1,
                    "{args:?} {name}: multi-block gate"
                );
            } else {
                // More than one stream is not the shape the gate admits.
                assert_eq!(count, None, "{args:?} {name}: multi-stream was admitted");
                assert!(!lzma_turbo::xz::is_single_stream_multi_block(&mut cur));
            }

            // And the parallel reader's own view of the file agrees with xz.
            let r = XzParallelReader::new(Cursor::new(&bytes)).expect("parallel reader");
            // `xz -l --robot` totals its `file` line over the whole file, so
            // these are the file's figures, not one stream's.
            assert_eq!(
                r.uncompressed_size(),
                uncompressed,
                "{args:?} {name}: uncompressed size"
            );
            assert_eq!(
                r.block_count() as u64,
                blocks,
                "{args:?} {name}: total blocks"
            );
        }
    }
}

#[test]
fn a_memory_limit_smaller_than_a_block_still_finishes() {
    // The trap this guards: a block whose header declares both sizes is
    // dispatched whole, so the decoder wants to buffer all of it - but under a
    // limit smaller than the block, `feed` will not take that much. Waiting
    // would wait forever, so such a block has to fall back to the chase.
    let payload = common::pseudo_random(2 << 20, 0x11ce);
    let Some(data) = xz_compress(&["-1", "--block-size=524288", "-T2"], &payload) else {
        return;
    };
    // The limit has to leave room for one block's dictionary, which is the
    // block's own 512 KiB here; below that the decode rightly fails. What is
    // tested is the range where a block fits but the whole padded block plus
    // its output does not.
    for limit in [640u64 << 10, 1 << 20, 2 << 20] {
        let opts = XzOptions::default()
            .with_threads(4)
            .with_memory_limit(limit);
        let mut dec = XzAdaptiveDecoder::new(opts);
        let mut out: Vec<u8> = Vec::new();
        let mut pos = 0usize;
        let mut idle = 0u32;
        loop {
            let before = pos;
            if pos < data.len() {
                let end = (pos + 100_000).min(data.len());
                pos += dec.feed(&data[pos..end]).expect("feed");
                if pos == data.len() {
                    dec.end_of_input();
                }
            }
            let mut wrote = false;
            let status = dec
                .drain(|off, bytes| {
                    wrote = true;
                    let off = usize::try_from(off).expect("offset");
                    if out.len() < off + bytes.len() {
                        out.resize(off + bytes.len(), 0u8);
                    }
                    out[off..off + bytes.len()].copy_from_slice(bytes);
                })
                .expect("drain");
            if status == DrainStatus::Finished {
                break;
            }
            idle = if wrote || pos != before { 0 } else { idle + 1 };
            assert!(idle < 100, "the decoder stopped making progress at {limit}");
        }
        assert_eq!(out, payload, "limit {limit}");
    }
}

#[test]
fn a_block_that_runs_out_of_its_declared_compressed_size_does_not_spin() {
    // Found by the fuzzer: a block header declaring a compressed size, whose
    // LZMA2 never reaches its end marker inside it. The decoder clamps its
    // input to the declared size, so it was offered bytes it could not use,
    // returned "nothing read, nothing written", and was offered them again.
    // The bytes after the declared size are padding and the check, so running
    // out of it without an end marker is a size mismatch, not a wait.
    let path = common::data_dir().join("spin.declared-size.xz");
    if !path.exists() {
        return;
    }
    let bytes = std::fs::read(&path).expect("read");
    assert!(decode(&bytes).is_err(), "the truncated block was accepted");
    assert!(decode_chunked(&bytes, 7).is_err());
    assert!(decode_adaptive(&bytes, 4, 11).is_err());
}

/// As [`decode_adaptive`], but takes the output `limit` bytes at a time.
fn decode_adaptive_upto(
    data: &[u8],
    threads: usize,
    chunk: usize,
    limit: usize,
) -> Result<Vec<u8>, lzma_turbo::XzError> {
    let opts = XzOptions::default().with_threads(threads);
    let mut dec = XzAdaptiveDecoder::new(opts);
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos < data.len() {
            let end = (pos + chunk).min(data.len());
            pos += dec.feed(&data[pos..end])?;
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let mut handed = 0usize;
        let status = dec.drain_upto(limit, |off, bytes| {
            assert_eq!(off, out.len() as u64, "out of order at {off}");
            assert!(
                bytes.len() <= limit,
                "sink got {} over {limit}",
                bytes.len()
            );
            handed += bytes.len();
            out.extend_from_slice(bytes);
        })?;
        assert!(handed <= limit, "{handed} bytes for a {limit} limit");
        match status {
            DrainStatus::Finished => return Ok(out),
            DrainStatus::NeedsMoreInput if pos == data.len() => {
                panic!("the decoder wanted input after the file ended")
            }
            _ => {}
        }
    }
}

#[test]
fn a_bounded_adaptive_drain_delivers_the_same_bytes() {
    for name in ["mixed.mb.xz", "mixed.bcjx86.xz", "tiny.concat.xz"] {
        let path = common::data_dir().join(name);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read");
        // The sequential reader is the reference here: `tiny.concat.xz` is two
        // streams, so what it decodes to is not one copy of a source file.
        let want = decode(&bytes).expect(name);
        for threads in [1usize, 4] {
            for limit in [1usize, 5, 997, 1 << 16] {
                let have = decode_adaptive_upto(&bytes, threads, 512, limit)
                    .unwrap_or_else(|e| panic!("{name} t={threads} limit={limit}: {e}"));
                assert_eq!(have, want, "{name} t={threads} limit={limit}");
            }
        }
    }
}

#[test]
fn the_block_table_locates_every_block_a_decode_produces() {
    // The seekable question a consumer asks before it commits to a decode:
    // where the blocks are and what they decode to, without decoding.
    for name in ["mixed.mb.xz", "tiny.concat.xz"] {
        let path = common::data_dir().join(name);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read");
        let mut src = Cursor::new(bytes.clone());
        let table = lzma_turbo::xz::block_table(&mut src, u64::MAX).expect(name);
        assert!(!table.is_empty(), "{name} has no blocks");

        let mut want_offset = 0u64;
        for b in &table {
            assert_eq!(b.uncompressed_offset, want_offset, "{name} block offsets");
            assert!(
                b.file_offset < bytes.len() as u64,
                "{name} block past the end of the file"
            );
            // The block header's first byte encodes its own size, so a located
            // block must start on one.
            let first = bytes[usize::try_from(b.file_offset).expect("offset")];
            assert_ne!(first, 0, "{name} points a block at an index indicator");
            want_offset += b.record.uncompressed_size;
        }
        let decoded = decode(&bytes).expect(name);
        assert_eq!(
            want_offset,
            decoded.len() as u64,
            "{name}: the table's sizes do not add up to the decode"
        );
    }
}

/// The claim the LZMA2 side had to be fixed to make: a bounded drain cannot
/// turn a truncated file into a finished one. The chase decoder stops wherever
/// the caller's budget runs out, which for a small budget is inside a block;
/// the block's padding, check and the stream's index are all still ahead of
/// it, so there is nothing for it to mistake for an end.
#[test]
fn a_bounded_adaptive_drain_still_rejects_a_truncated_file() {
    let plain = common::pseudo_random(1 << 20, 0x5eed);
    let Some(packed) = xz_compress(&["-3", "-T1"], &plain) else {
        return;
    };
    for cut in [packed.len() / 3, packed.len() / 2, packed.len() - 8] {
        for limit in [1usize, 7, 65_536] {
            let err = decode_adaptive_upto(&packed[..cut], 1, 4096, limit);
            assert!(
                err.is_err(),
                "cut {cut} limit {limit}: a truncated file was accepted"
            );
        }
    }
}
