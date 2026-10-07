//! The `.xz` writer: what it produces must come back through every reader
//! this crate has, and through `xz` itself.
//!
//! The parity tests prove the compressed data is bit-exact with the SDK's
//! encoder. Nothing proves the *frame* that way, because the SDK's `.xz`
//! writer is not what was ported, so it is proved the other way round: by
//! decoding.

#![cfg(all(feature = "enc", feature = "xz"))]

use std::io::{Cursor, Read, Write};
use std::process::{Command, Stdio};

use lzma_turbo::xz::bcj::BcjKind;
use lzma_turbo::xz::{
    CheckType, FILTER_DELTA, FilterFlags, XzAdaptiveDecoder, XzOptions, XzParallelReader, XzReader,
};
use lzma_turbo::{
    DrainStatus, LzmaEncProps, LzmaWriter, XzWriter, encode_lzma_alone, encode_xz,
    encode_xz_with_filters,
};

const BCJ_KINDS: [(BcjKind, &str); 8] = [
    (BcjKind::X86, "x86"),
    (BcjKind::Ppc, "ppc"),
    (BcjKind::Ia64, "ia64"),
    (BcjKind::Arm, "arm"),
    (BcjKind::ArmThumb, "armt"),
    (BcjKind::Sparc, "sparc"),
    (BcjKind::Arm64, "arm64"),
    (BcjKind::RiscV, "riscv"),
];

/// Every chain the writer is asked to produce: each BCJ kind on its own, a
/// couple of delta distances, and delta behind a BCJ filter.
fn chains() -> Vec<(String, Vec<FilterFlags>)> {
    let mut v: Vec<(String, Vec<FilterFlags>)> = vec![("plain".into(), Vec::new())];
    for (kind, name) in BCJ_KINDS {
        v.push((
            name.to_owned(),
            vec![FilterFlags::new(kind.filter_id(), &[]).unwrap()],
        ));
        // The four-byte form, with a start offset the kind allows.
        v.push((
            format!("{name}-offset"),
            vec![
                FilterFlags::new(kind.filter_id(), &(kind.alignment() * 2).to_le_bytes()).unwrap(),
            ],
        ));
    }
    for distance in [1u8, 3, 255] {
        v.push((
            format!("delta-{}", u32::from(distance) + 1),
            vec![FilterFlags::new(FILTER_DELTA, &[distance]).unwrap()],
        ));
    }
    v.push((
        "delta-then-x86".into(),
        vec![
            FilterFlags::new(FILTER_DELTA, &[3]).unwrap(),
            FilterFlags::new(BcjKind::X86.filter_id(), &[]).unwrap(),
        ],
    ));
    v
}

mod corpus;
use corpus::{corpus, tempdir};

/// Every check type this build can write.
fn checks() -> Vec<CheckType> {
    let mut v = vec![CheckType::None, CheckType::Crc32, CheckType::Crc64];
    if cfg!(any(feature = "crypto", feature = "native-crypto")) {
        v.push(CheckType::Sha256);
    }
    v
}

/// Drives [`XzAdaptiveDecoder`] over a whole stream, as `tests/xz_utils.rs`
/// does.
fn adaptive(data: &[u8], threads: usize, chunk: usize, what: &str) -> Vec<u8> {
    let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(threads));
    let mut out = Vec::new();
    let mut pos = 0;
    if data.is_empty() {
        dec.end_of_input();
    }
    loop {
        if pos < data.len() {
            pos += dec
                .feed(&data[pos..(pos + chunk).min(data.len())])
                .unwrap_or_else(|e| panic!("{what}: feed: {e}"));
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let status = dec
            .drain(|off, bytes| {
                let off = usize::try_from(off).expect("offset");
                if out.len() < off + bytes.len() {
                    out.resize(off + bytes.len(), 0);
                }
                out[off..off + bytes.len()].copy_from_slice(bytes);
            })
            .unwrap_or_else(|e| panic!("{what}: drain: {e}"));
        match status {
            DrainStatus::Finished => return out,
            DrainStatus::NeedsMoreInput if pos == data.len() => {
                panic!("{what}: the adaptive decoder wanted input after the stream ended")
            }
            _ => {}
        }
    }
}

/// Puts one stream through all three readers.
fn decode_every_way(xz: &[u8], want: &[u8], what: &str) {
    let mut out = Vec::new();
    XzReader::new(Cursor::new(xz))
        .read_to_end(&mut out)
        .unwrap_or_else(|e| panic!("{what}: XzReader: {e}"));
    assert_eq!(out, want, "{what}: XzReader");

    let mut out = Vec::new();
    XzParallelReader::with_options(Cursor::new(xz), XzOptions::default().with_threads(4))
        .unwrap_or_else(|e| panic!("{what}: XzParallelReader: {e}"))
        .read_to_end(&mut out)
        .unwrap_or_else(|e| panic!("{what}: XzParallelReader: {e}"));
    assert_eq!(out, want, "{what}: XzParallelReader");

    assert_eq!(
        adaptive(xz, 1, 4096, what),
        want,
        "{what}: adaptive, 1 thread"
    );
    // A byte-at-a-time feed is the interesting case for the adaptive
    // decoder's state machine and a slow one for a large stream, so the
    // small inputs carry it.
    if xz.len() < 64 * 1024 {
        assert_eq!(
            adaptive(xz, 4, 7, what),
            want,
            "{what}: adaptive, 4 threads, 7-byte feeds"
        );
    }
}

#[test]
fn every_check_and_block_size_round_trips_through_every_reader() {
    for (name, data) in corpus() {
        for check in checks() {
            for block_size in [0u64, 4096, 1 << 16] {
                let props = LzmaEncProps::new().with_level(3).with_dict_size(1 << 16);
                let xz = encode_xz(&data, &props, check, block_size).expect("encode");
                decode_every_way(
                    &xz,
                    &data,
                    &format!("{name}, check {check:?}, block {block_size}"),
                );
            }
        }
    }
}

#[test]
fn the_writer_adapters_produce_the_same_bytes_as_the_one_shot_calls() {
    let props = LzmaEncProps::new().with_level(4);
    let data: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();

    let mut w = XzWriter::new(Vec::new(), &props).expect("writer");
    w.set_check(CheckType::Crc64).expect("check");
    w.set_block_size(1 << 16);
    for chunk in data.chunks(1777) {
        w.write_all(chunk).expect("write");
    }
    let streamed = w.finish().expect("finish");
    let one_shot = encode_xz(&data, &props, CheckType::Crc64, 1 << 16).expect("encode");
    assert_eq!(streamed, one_shot, "streaming must not change the stream");
    decode_every_way(&streamed, &data, "XzWriter");

    let mut w = LzmaWriter::new(Vec::new(), &props);
    for chunk in data.chunks(1777) {
        w.write_all(chunk).expect("write");
    }
    assert_eq!(
        w.finish().expect("finish"),
        encode_lzma_alone(&data, &props).expect("encode"),
        "LzmaWriter"
    );
}

/// One of the external decoders: where it is, and whether it must be there.
///
/// `LZMA_TURBO_XZ` and `LZMA_TURBO_7ZZ` name the binary, which is how CI
/// points at the pinned builds `.github/scripts/install-xz.sh` and
/// `cargo xtask sevenzip` install rather than at whatever the image happens to
/// carry; without one the plain name is looked up on `PATH`. The matching
/// `_REQUIRE` variable turns "not installed" from a skip into a failure, as
/// `LZMA_TURBO_LZMA_UTIL_REQUIRE` already does for the parity oracles: an
/// external decoder that quietly is not run proves nothing, and the writer is
/// exactly the part of this crate that has no oracle of its own.
struct External {
    var: &'static str,
    default: &'static str,
}

const XZ: External = External {
    var: "LZMA_TURBO_XZ",
    default: "xz",
};
const SEVENZIP: External = External {
    var: "LZMA_TURBO_7ZZ",
    default: "7zz",
};

impl External {
    /// The binary to run, or `None` with a message - unless the `_REQUIRE`
    /// variable is set, and then a missing decoder is a failure.
    fn find(&self) -> Option<String> {
        let tool = std::env::var(self.var).unwrap_or_else(|_| self.default.to_owned());
        // `7zz` with no argument prints its banner and exits non-zero, so the
        // question is whether the process starts at all, not what it says.
        let runs = Command::new(&tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if runs {
            return Some(tool);
        }
        assert!(
            std::env::var_os(format!("{}_REQUIRE", self.var)).is_none(),
            "{}_REQUIRE is set but {tool} will not run",
            self.var
        );
        eprintln!("skipping: `{tool}` is not on PATH");
        None
    }
}

/// Runs `tool` with `args` over `input` on stdin and returns stdout.
///
/// The write goes on its own thread: the decompressed output is much larger
/// than the input, so a parent that writes it all before reading fills the
/// child's stdout pipe and both ends stop.
fn pipe(tool: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let owned = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let out = child.wait_with_output().expect("wait");
    writer.join().expect("writer");
    out.status.success().then_some(out.stdout)
}

#[test]
fn every_filter_chain_round_trips_through_every_reader() {
    let props = LzmaEncProps::new().with_level(3).with_dict_size(1 << 16);
    for (tag, filters) in chains() {
        for (name, data) in corpus() {
            for block in [0u64, 4096] {
                let xz = encode_xz_with_filters(&data, &props, CheckType::Crc64, block, &filters)
                    .expect("encode");
                decode_every_way(&xz, &data, &format!("{tag} {name} block={block}"));
            }
        }
    }
}

#[test]
fn a_chain_the_format_forbids_is_refused() {
    let props = LzmaEncProps::new();
    let lzma2 = FilterFlags::new(0x21, &[0]).unwrap();
    // LZMA2 is a last-only filter, so it may not appear among the others.
    assert!(encode_xz_with_filters(b"x", &props, CheckType::Crc32, 0, &[lzma2]).is_err());
    // A misaligned BCJ start offset (spec 5.3.2).
    let bad = FilterFlags::new(BcjKind::Arm64.filter_id(), &1u32.to_le_bytes()).unwrap();
    assert!(encode_xz_with_filters(b"x", &props, CheckType::Crc32, 0, &[bad]).is_err());
    // More non-last filters than a chain may hold.
    let delta = FilterFlags::new(FILTER_DELTA, &[0]).unwrap();
    assert!(
        encode_xz_with_filters(
            b"x",
            &props,
            CheckType::Crc32,
            0,
            &[delta, delta, delta, delta]
        )
        .is_err()
    );
}

#[test]
fn xz_itself_accepts_what_this_crate_writes() {
    let Some(xz_tool) = XZ.find() else {
        return;
    };
    let dir = tempdir("xz-encoder");
    for (name, data) in corpus() {
        for check in checks() {
            let props = LzmaEncProps::new().with_level(3).with_dict_size(1 << 16);
            let xz = encode_xz(&data, &props, check, 4096).expect("encode");

            // `xz -t` checks the framing, the index and the checks without
            // producing output.
            let path = dir.join(format!("{name}-{check:?}.xz"));
            std::fs::write(&path, &xz).expect("write");
            let status = Command::new(&xz_tool)
                .arg("-t")
                .arg(&path)
                .status()
                .expect("run xz -t");
            assert!(status.success(), "xz -t rejected {name}, check {check:?}");

            let got = pipe(&xz_tool, &["-dc"], &xz)
                .unwrap_or_else(|| panic!("xz -dc failed on {name}, check {check:?}"));
            assert_eq!(got, data, "xz -dc on {name}, check {check:?}");
        }

        // The `.lzma` writer goes through the same tool, which reads
        // LZMA-Alone with `--format=lzma`.
        let alone = encode_lzma_alone(&data, &LzmaEncProps::new()).expect("encode");
        let got = pipe(&xz_tool, &["-dc", "--format=lzma"], &alone)
            .unwrap_or_else(|| panic!("xz -dc --format=lzma failed on {name}"));
        assert_eq!(got, data, "xz -dc --format=lzma on {name}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn xz_itself_accepts_every_filter_chain() {
    let Some(xz_tool) = XZ.find() else {
        return;
    };
    let dir = tempdir("xz-filters");
    let props = LzmaEncProps::new().with_level(3).with_dict_size(1 << 16);
    // One input big enough to exercise the filters, rather than the whole
    // corpus times every chain, which `xz` would spend minutes on.
    let (_, data) = corpus()
        .into_iter()
        .find(|(n, _)| n == "text-big")
        .expect("corpus case");
    for (tag, filters) in chains() {
        let xz =
            encode_xz_with_filters(&data, &props, CheckType::Crc64, 0, &filters).expect("encode");
        let path = dir.join(format!("{tag}.xz"));
        std::fs::write(&path, &xz).expect("write");
        let status = Command::new(&xz_tool)
            .arg("-t")
            .arg(&path)
            .status()
            .expect("run xz -t");
        assert!(status.success(), "xz -t rejected the {tag} chain");
        let got = pipe(&xz_tool, &["-dc"], &xz).unwrap_or_else(|| panic!("xz -dc failed on {tag}"));
        assert_eq!(got, data, "xz -dc on the {tag} chain");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seven_zip_accepts_what_this_crate_writes() {
    let Some(sevenzip) = SEVENZIP.find() else {
        return;
    };
    let dir = tempdir("7zz-encoder");
    for (name, data) in corpus() {
        let props = LzmaEncProps::new().with_level(3).with_dict_size(1 << 16);
        let path = dir.join(format!("{name}.xz"));
        std::fs::write(
            &path,
            encode_xz(&data, &props, CheckType::Crc64, 0).expect("encode"),
        )
        .expect("write");
        let out = Command::new(&sevenzip)
            .arg("t")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run 7zz");
        assert!(out.success(), "7zz t rejected {name}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A deterministic stand-in for machine code, `len` bytes long.
///
/// A library of short x86-flavoured routines - a few common opcodes, small
/// operands, `E8` calls to other routines - is laid out again and again in a
/// shuffled order, so the same routine recurs at distances well beyond a
/// 256 KiB dictionary and every copy of a call carries a different relative
/// target, which is what the x86 converter turns back into one absolute
/// address. Nothing in it is random at run time: one seed, one payload.
fn code_like(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    const OPS: [u8; 12] = [
        0x48, 0x89, 0x8b, 0x83, 0x85, 0x0f, 0x74, 0x75, 0x31, 0xc3, 0x5d, 0x41,
    ];
    // Each routine is a list of pieces: plain bytes, or a call to routine `n`.
    enum Piece {
        Bytes(Vec<u8>),
        Call(usize),
    }
    let routines = 3000;
    let mut library: Vec<Vec<Piece>> = Vec::with_capacity(routines);
    for _ in 0..routines {
        let mut pieces = vec![Piece::Bytes(vec![0x55, 0x48, 0x89, 0xe5])];
        let steps = 8 + (next() % 40) as usize;
        for _ in 0..steps {
            let r = next();
            if r % 5 == 0 {
                pieces.push(Piece::Call((r >> 8) as usize % routines));
            } else {
                let n = 2 + (r >> 8) as usize % 6;
                let bytes = (0..n)
                    .map(|i| {
                        let v = next();
                        if i == 0 {
                            OPS[v as usize % OPS.len()]
                        } else {
                            (v >> 16) as u8 & 0x3f
                        }
                    })
                    .collect();
                pieces.push(Piece::Bytes(bytes));
            }
        }
        pieces.push(Piece::Bytes(vec![0x5d, 0xc3]));
        library.push(pieces);
    }
    // Where each routine "lives", as a call target.
    let home: Vec<u32> = (0..routines).map(|i| (i as u32) * 256 + 0x1000).collect();

    let mut out = Vec::with_capacity(len + 4096);
    let mut order: Vec<usize> = (0..routines).collect();
    while out.len() < len {
        // A fresh shuffle each pass, so a routine's neighbours change.
        for i in (1..order.len()).rev() {
            order.swap(i, next() as usize % (i + 1));
        }
        for &r in &order {
            for piece in &library[r] {
                match piece {
                    Piece::Bytes(b) => out.extend_from_slice(b),
                    Piece::Call(t) => {
                        let after = out.len() as u32 + 5;
                        out.push(0xe8);
                        out.extend_from_slice(&home[*t].wrapping_sub(after).to_le_bytes());
                    }
                }
            }
            if out.len() >= len {
                break;
            }
        }
    }
    out.truncate(len);
    out
}

#[test]
fn xz_preset_is_liblzma_preset_table() {
    // `lzma_lzma_preset` in XZ Utils 5.8: dictionary, mode, match finder,
    // nice length and depth for each level, with the depth `lz_encoder.c`
    // derives for a binary tree when the preset leaves it at zero.
    let dict = [18u32, 20, 21, 22, 22, 23, 23, 24, 25, 26];
    let nice = [128u32, 128, 273, 273, 16, 32, 64, 64, 64, 64];
    let depth = [4u32, 8, 24, 48, 24, 32, 48, 48, 48, 48];
    for preset in 0..=9u32 {
        let p = LzmaEncProps::xz_preset(preset, false).expect("preset");
        let n = p.normalized();
        let i = preset as usize;
        assert_eq!(n.dict_size, 1 << dict[i], "preset {preset} dictionary");
        assert_eq!((n.lc, n.lp, n.pb), (3, 0, 2), "preset {preset} lc/lp/pb");
        assert_eq!(n.fb, nice[i], "preset {preset} nice length");
        assert_eq!(n.mc, depth[i], "preset {preset} depth");
        // Levels 0-3 are the fast mode over a hash chain, the rest the normal
        // mode over a binary tree with a four-byte hash.
        assert_eq!(n.bt_mode, u32::from(preset > 3), "preset {preset} finder");
        assert_eq!(n.num_hash_bytes, 4, "preset {preset} hash bytes");
    }
    for preset in 0..=9u32 {
        let n = LzmaEncProps::xz_preset(preset, true)
            .expect("preset")
            .normalized();
        let (fb, mc) = if preset == 3 || preset == 5 {
            (192, 112)
        } else {
            (273, 512)
        };
        assert_eq!((n.fb, n.mc, n.bt_mode), (fb, mc, 1), "preset {preset}e");
        assert_eq!(n.dict_size, 1 << dict[preset as usize], "preset {preset}e");
    }
    assert!(LzmaEncProps::xz_preset(10, false).is_err());
}

#[test]
fn bcj_x86_at_preset_1_costs_no_more_than_no_filter() {
    let data = code_like(3 << 20, 0x5eed_c0de);
    let x86 = [FilterFlags::new(BcjKind::X86.filter_id(), &[]).unwrap()];
    for (what, props) in [
        ("xz preset 1", LzmaEncProps::xz_preset(1, false).unwrap()),
        ("SDK level 1", LzmaEncProps::new().with_level(1)),
    ] {
        let plain = encode_xz(&data, &props, CheckType::Crc64, 0).expect("encode");
        let filtered =
            encode_xz_with_filters(&data, &props, CheckType::Crc64, 0, &x86).expect("encode");
        // On code the converter should help, and must never cost more than a
        // tenth: the filter is a pass over the data, not a different encoder.
        assert!(
            filtered.len() * 10 <= plain.len() * 11,
            "{what}: x86 + LZMA2 {} bytes against {} without the filter",
            filtered.len(),
            plain.len()
        );
        decode_every_way(&filtered, &data, what);
    }
}

#[test]
fn xz_preset_1_writes_what_xz_1_writes_within_a_few_percent() {
    let Some(xz_tool) = XZ.find() else {
        return;
    };
    let data = code_like(3 << 20, 0x5eed_c0de);
    let props = LzmaEncProps::xz_preset(1, false).unwrap();
    let x86 = FilterFlags::new(BcjKind::X86.filter_id(), &[]).unwrap();
    for (what, filters, args) in [
        ("plain", Vec::new(), vec!["-zc", "-T1", "--lzma2=preset=1"]),
        (
            "x86",
            vec![x86],
            vec!["-zc", "-T1", "--x86", "--lzma2=preset=1"],
        ),
    ] {
        let ours =
            encode_xz_with_filters(&data, &props, CheckType::Crc64, 0, &filters).expect("encode");
        let theirs = pipe(&xz_tool, &args, &data).unwrap_or_else(|| panic!("xz failed on {what}"));
        assert!(
            ours.len() * 100 <= theirs.len() * 105,
            "{what}: {} bytes against xz's {}",
            ours.len(),
            theirs.len()
        );
        let back = pipe(&xz_tool, &["-dc"], &ours).unwrap_or_else(|| panic!("xz -dc on {what}"));
        assert_eq!(back, data, "xz -dc on {what}");
        let mut out = Vec::new();
        XzReader::new(Cursor::new(&theirs))
            .read_to_end(&mut out)
            .unwrap_or_else(|e| panic!("{what}: XzReader on xz's stream: {e}"));
        assert_eq!(out, data, "{what}: XzReader on xz's stream");
    }
}
