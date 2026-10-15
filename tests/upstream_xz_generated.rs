//! Generated corpus and filter matrix from XZ Utils 5.8.3 (0BSD):
//! tests/create_compress_files.c and tests/test_compress.sh.
#![cfg(all(feature = "enc", feature = "xz"))]

use lzma_turbo::xz::{CheckType, FilterFlags, XzOptions, bcj::BcjKind};
use lzma_turbo::{LzmaEncProps, XzParallelReader, XzReader, encode_xz_with_filters};
use std::io::{Cursor, Read};

fn generated() -> Vec<(&'static str, Vec<u8>)> {
    let abc = b"abc\n".repeat(12345);
    let mut state = 5u32;
    let mut random = Vec::with_capacity(123456 * 4);
    for _ in 0..123456 {
        state = state.wrapping_mul(101771).wrapping_add(71777);
        random.extend_from_slice(&state.to_le_bytes());
    }
    const WORDS: &str = "Lorem ipsum dolor sit amet, consectetur adipisicing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
    let words: Vec<_> = WORDS.split(' ').collect();
    let mut text = Vec::new();
    let append = |out: &mut Vec<u8>, word: &str, index: usize| {
        out.extend_from_slice(word.as_bytes());
        out.push(b' ');
        if index % 7 == 6 {
            out.push(b'\n');
        }
    };
    for (i, word) in words.iter().enumerate() {
        append(&mut text, word, i);
    }
    state = 29;
    for _ in 0..500 {
        text.extend_from_slice(b"\n\n");
        for i in 0..words.len() {
            state = state.wrapping_mul(101771).wrapping_add(71777);
            append(&mut text, words[state as usize % words.len()], i);
        }
    }
    vec![("abc", abc), ("random", random), ("text", text)]
}

#[test]
fn upstream_generated_corpus_presets_and_filter_matrix() {
    let mut cases = Vec::new();
    for level in 1..=4 {
        cases.push((LzmaEncProps::xz_preset(level, false).unwrap(), Vec::new()));
    }
    let props = LzmaEncProps::new().with_level(1).with_dict_size(64 * 1024);
    for distance in [1u16, 4, 256] {
        cases.push((
            props,
            vec![FilterFlags::new(3, &[(distance - 1) as u8]).unwrap()],
        ));
    }
    for kind in [
        BcjKind::X86,
        BcjKind::Ppc,
        BcjKind::Ia64,
        BcjKind::Arm,
        BcjKind::ArmThumb,
        BcjKind::Arm64,
        BcjKind::Sparc,
        BcjKind::RiscV,
    ] {
        cases.push((
            props,
            vec![FilterFlags::new(kind.filter_id(), &[]).unwrap()],
        ));
    }
    for (name, source) in generated() {
        for (case, (props, filters)) in cases.iter().enumerate() {
            let encoded =
                encode_xz_with_filters(&source, props, CheckType::Crc64, 0, filters).unwrap();
            let mut decoded = Vec::new();
            XzReader::new(&encoded[..])
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded, source, "{name} case {case} sequential");
            decoded.clear();
            XzParallelReader::with_options(
                Cursor::new(&encoded),
                XzOptions::default().with_threads(2),
            )
            .unwrap()
            .read_to_end(&mut decoded)
            .unwrap();
            assert_eq!(decoded, source, "{name} case {case} parallel");
        }
    }
}
