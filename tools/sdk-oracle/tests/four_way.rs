//! lzma-turbo against the LZMA SDK it was ported from.
//!
//! Up to four decoders read every stream: the crate's assembly loop, the
//! crate's portable loop, the SDK's C loop and the SDK's assembly loop, the
//! last two built by `build.rs` from a pinned SDK checkout. The streams are
//! the committed vectors, every truncation of them, bit-flipped copies and
//! random bytes behind valid headers, each fed in several input and output
//! slice sizes. `LzmaDecoder::decode` and `Lzma2Decoder::decode` port
//! `LzmaDec_DecodeToBuf` and `Lzma2Dec_DecodeToBuf`, so the decoders are
//! compared call by call: every call must read and write the same number of
//! bytes and stop with the same status or fail with the same error, and the
//! output must be the same bytes. Where the SDK's assembly does not build (on
//! macOS, and on arm64 Windows) three decoders run; without `LZMA_SDK` the
//! tests print that the oracle is absent and pass.

#[path = "../../../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::{LZMA_VARIANTS, SOURCES, XZ_VARIANTS, pseudo_random, xz_lzma2_block};
use sdk_oracle::{
    VARIANTS,
    lockstep::{Shape, Trace, Which, trace_lzma, trace_lzma2},
};

/// Input and output slice sizes. The one-byte pairs keep the decoders in the
/// `tempBuf` single-symbol path, where the loop is entered with
/// `bufLimit == buf`; the large ones let it run for millions of symbols.
const CHUNKS: &[Shape] = &[
    Shape::new(1, 1),
    Shape::new(1, 4096),
    Shape::new(7, 13),
    Shape::new(20, 1),
    Shape::new(4096, 4096),
    Shape::new(usize::MAX, 1 << 20),
];

fn decoders() -> Option<Vec<Which>> {
    if VARIANTS.is_empty() {
        eprintln!(
            "LZMA_SDK was not set when this crate was built; there is no SDK oracle to compare against"
        );
        return None;
    }
    Some(sdk_oracle::lockstep::decoders())
}

fn agree(what: &str, decoders: &[Which], run: impl Fn(Which) -> Trace) {
    if let Err(e) = sdk_oracle::lockstep::agree(decoders, run) {
        panic!("{what}: {e}");
    }
}

fn data(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/data")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every prefix of the first 512 bytes, then 256 cuts across the rest; the
/// same points `tests/asm_parity.rs` cuts at.
fn truncation_points(len: usize) -> impl Iterator<Item = usize> {
    let dense = len.min(512);
    let stride = (len.saturating_sub(dense) / 256).max(1);
    (0..dense).chain((dense..len).step_by(stride))
}

fn lzma_vectors() -> impl Iterator<Item = (String, Vec<u8>)> {
    SOURCES.iter().flat_map(|stem| {
        LZMA_VARIANTS.iter().map(move |variant| {
            let name = format!("{stem}.{variant}.lzma");
            let bytes = data(&name);
            (name, bytes)
        })
    })
}

fn lzma2_vectors() -> impl Iterator<Item = (String, u8, Vec<u8>)> {
    SOURCES.iter().flat_map(|stem| {
        XZ_VARIANTS.iter().filter_map(move |variant| {
            let name = format!("{stem}.{variant}.xz");
            let raw = data(&name);
            let (prop, block) = xz_lzma2_block(&raw)?;
            Some((name, prop, block.to_vec()))
        })
    })
}

#[test]
fn positional_ring_runs_match_the_sdk_vectors() {
    let Some(_) = decoders() else { return };
    use lzma_turbo::{Lzma2RunScanner, run::Lzma2RunDecoder};
    use std::sync::atomic::AtomicBool;

    for (name, prop, packed) in lzma2_vectors() {
        let source = data(&format!("src_{}.bin", name.split('.').next().unwrap()));
        let mut scanner = Lzma2RunScanner::new();
        scanner.feed(&packed).unwrap();
        assert!(scanner.finished(), "{name}");
        let mut actual = Vec::new();
        while let Some(run) = scanner.next_run() {
            let mut decoder = Lzma2RunDecoder::new(0, prop, run, u64::MAX).unwrap();
            let image = &packed[run.in_offset as usize..(run.in_offset + run.packed_len) as usize];
            let cancel = AtomicBool::new(false);
            let mut position = 0;
            loop {
                let end = (position + 4093).min(image.len());
                let progress = decoder
                    .decode(
                        &image[position..end],
                        37019,
                        &cancel,
                        &mut |offset, bytes: &[u8]| {
                            assert_eq!(offset, actual.len() as u64);
                            let size = bytes.len().min(8191);
                            actual.extend_from_slice(&bytes[..size]);
                            Ok(size)
                        },
                    )
                    .unwrap();
                position += progress.read;
                if progress.read == 0 && progress.written == 0 {
                    assert_eq!(position, image.len());
                    break;
                }
            }
            decoder.finish(&cancel).unwrap();
        }
        assert_eq!(actual, source, "{name}");
        for &variant in VARIANTS {
            let trace = trace_lzma2(Which::Sdk(variant), prop, &packed, Shape::new(4093, 37019));
            assert!(trace.calls.iter().all(Result::is_ok), "{variant:?} {name}");
            assert_eq!(actual, trace.output, "{variant:?} {name}");
        }
    }
}

#[test]
fn the_sdk_oracle_decodes_the_vectors_to_their_sources() {
    // The oracle is checked against the plaintext, not only against the
    // crate: a broken oracle build must not pass for a working one.
    let Some(_) = decoders() else { return };
    for &variant in VARIANTS {
        for (name, stream) in lzma_vectors() {
            let source = data(&format!("src_{}.bin", name.split('.').next().unwrap()));
            let trace = trace_lzma(
                Which::Sdk(variant),
                &stream,
                Shape::new(usize::MAX, 1 << 20),
            );
            assert!(trace.calls.iter().all(Result::is_ok), "{variant:?} {name}");
            assert!(
                trace.output == source,
                "{variant:?} {name}: output is not the source"
            );
        }
    }
}

#[test]
fn every_decoder_agrees_on_the_vectors() {
    let Some(decoders) = decoders() else { return };
    for (name, stream) in lzma_vectors() {
        for &shape in CHUNKS {
            agree(&format!("{name} {shape:?}"), &decoders, |w| {
                trace_lzma(w, &stream, shape)
            });
        }
    }
    for (name, prop, block) in lzma2_vectors() {
        for &shape in CHUNKS {
            agree(&format!("{name} {shape:?}"), &decoders, |w| {
                trace_lzma2(w, prop, &block, shape)
            });
        }
    }
}

#[test]
fn every_decoder_agrees_on_every_truncation() {
    let Some(decoders) = decoders() else { return };
    for (name, stream) in lzma_vectors() {
        for cut in truncation_points(stream.len()).filter(|&c| c >= 13) {
            agree(&format!("{name} cut at {cut}"), &decoders, |w| {
                trace_lzma(w, &stream[..cut], Shape::new(usize::MAX, 1 << 16))
            });
        }
    }
    for (name, prop, block) in lzma2_vectors() {
        for cut in truncation_points(block.len()) {
            agree(&format!("{name} cut at {cut}"), &decoders, |w| {
                trace_lzma2(w, prop, &block[..cut], Shape::new(usize::MAX, 1 << 16))
            });
        }
    }
}

#[test]
fn every_decoder_agrees_on_corrupt_streams() {
    let Some(decoders) = decoders() else { return };
    let mut rng = 0x0DDB_A11C_0FFE_E5EDu64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for (name, stream) in lzma_vectors().filter(|(n, _)| n.contains(".p1.")) {
        if stream.len() <= 13 {
            continue;
        }
        for round in 0..300 {
            let mut bad = stream.clone();
            let flips = 1 + round % 3;
            for _ in 0..flips {
                let at = 13 + (next() as usize) % (bad.len() - 13);
                bad[at] ^= 1 << (next() % 8);
            }
            let shape = CHUNKS[round % CHUNKS.len()];
            agree(&format!("{name} round {round}"), &decoders, |w| {
                trace_lzma(w, &bad, shape)
            });
        }
    }
    for (name, prop, block) in lzma2_vectors() {
        for round in 0..300 {
            let mut bad = block.clone();
            let at = (next() as usize) % bad.len();
            bad[at] ^= 1 << (next() % 8);
            let shape = CHUNKS[round % CHUNKS.len()];
            agree(&format!("{name} round {round}"), &decoders, |w| {
                trace_lzma2(w, prop, &bad, shape)
            });
        }
    }
}

#[test]
fn every_decoder_agrees_on_random_bytes_behind_a_valid_header() {
    let Some(decoders) = decoders() else { return };
    for seed in 1..=400u64 {
        let len = 1 + (seed as usize * 37) % 3000;
        let body = pseudo_random(len, seed);
        // lc=3 lp=0 pb=2 and a 64 KiB dictionary, size unknown, then noise.
        let mut stream = vec![0x5D, 0x00, 0x00, 0x01, 0x00];
        stream.extend_from_slice(&u64::MAX.to_le_bytes());
        stream.extend_from_slice(&body);
        let shape = CHUNKS[seed as usize % CHUNKS.len()];
        agree(&format!("lzma seed {seed}"), &decoders, |w| {
            trace_lzma(w, &stream, shape)
        });

        // An LZMA chunk header (reset everything, new props) over the same noise.
        let unpacked = (len as u16).wrapping_mul(3);
        let mut lzma2 = vec![
            0xE0,
            (unpacked >> 8) as u8,
            unpacked as u8,
            ((len - 1) >> 8) as u8,
            (len - 1) as u8,
            0x5D,
        ];
        lzma2.extend_from_slice(&body);
        lzma2.push(0);
        agree(&format!("lzma2 seed {seed}"), &decoders, |w| {
            trace_lzma2(w, 16, &lzma2, shape)
        });
    }
}

#[test]
fn every_decoder_agrees_on_every_property_byte() {
    let Some(decoders) = decoders() else { return };
    // A tiny stream so that the decoders that accept the properties also
    // decode something; what matters is who accepts what.
    let body = pseudo_random(64, 7);
    for props in 0..=255u8 {
        for dict in [0u32, 1, 4096, 1 << 16] {
            let mut stream = vec![props];
            stream.extend_from_slice(&dict.to_le_bytes());
            stream.extend_from_slice(&u64::MAX.to_le_bytes());
            stream.extend_from_slice(&body);
            agree(&format!("props {props:#04x} dict {dict}"), &decoders, |w| {
                trace_lzma(w, &stream, Shape::new(usize::MAX, 4096))
            });
        }
    }
    // LZMA2 dictionary properties; 28 to 40 ask for 64 MiB to 4 GiB and are
    // skipped only to keep the test's memory down.
    for prop in (0..=27u8).chain(41..=255) {
        agree(&format!("lzma2 dict prop {prop}"), &decoders, |w| {
            trace_lzma2(
                w,
                prop,
                &[0x01, 0x00, 0x00, 0x42, 0x00],
                Shape::new(usize::MAX, 4096),
            )
        });
    }
}
