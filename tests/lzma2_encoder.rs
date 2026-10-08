//! The LZMA2 encoder: bit-exactness against the reference, and round trips
//! back through this crate's own LZMA2 decoders.
//!
//! The reference side needs `cargo xtask lzma-util`, exactly as
//! `tests/lzma_parity.rs` does, and skips with a message when it is missing
//! unless `LZMA_TURBO_LZMA_UTIL_REQUIRE` is set.

#![cfg(all(feature = "std", feature = "enc"))]

mod corpus;

use std::{
    io::{Read, Write},
    process::Command,
};

use corpus::{corpus, max_len, mixed, tempdir, tool};
use lzma_turbo::{Lzma2Encoder, Lzma2Reader, Lzma2Writer, LzmaEncProps, MatchFinderKind};

/// The arguments `lzma2-oracle` takes, in its order.
fn oracle_args(props: &LzmaEncProps) -> Vec<String> {
    let n = props.normalized();
    [
        n.level,
        n.bt_mode,
        n.num_hash_bytes,
        n.lc,
        n.lp,
        n.pb,
        n.fb,
        n.dict_size,
    ]
    .iter()
    .map(u32::to_string)
    .collect()
}

fn settings() -> Vec<LzmaEncProps> {
    let mut out = Vec::new();
    for level in [0, 1, 5, 6, 9] {
        out.push(LzmaEncProps::new().with_level(level));
    }
    for kind in [
        MatchFinderKind::Hc4,
        MatchFinderKind::Hc5,
        MatchFinderKind::Bt2,
        MatchFinderKind::Bt3,
        MatchFinderKind::Bt4,
        MatchFinderKind::Bt5,
    ] {
        out.push(
            LzmaEncProps::new()
                .with_level(6)
                .with_match_finder(kind)
                .with_dict_size(1 << 18),
        );
    }
    // lc + lp must stay within LZMA2's limit of 4.
    for (lc, lp, pb) in [(0, 0, 0), (0, 4, 0), (4, 0, 4), (2, 2, 1)] {
        out.push(
            LzmaEncProps::new()
                .with_level(5)
                .with_lclppb(lc, lp, pb)
                .with_dict_size(1 << 16),
        );
    }
    out
}

#[test]
fn lzma2_round_trips_through_the_decoder() {
    for props in settings() {
        for (name, src) in corpus() {
            let mut enc = Lzma2Encoder::new(&props).expect("encoder");
            let encoded = enc.encode_to_vec(&src).expect("encode");
            let mut reader =
                Lzma2Reader::new(std::io::Cursor::new(&encoded), enc.properties()).expect("reader");
            let mut out = Vec::new();
            reader.read_to_end(&mut out).expect("decode");
            assert!(out == src, "round trip lost bytes on {name}");
        }
    }
}

#[test]
fn lzma2_rejects_lc_plus_lp_above_four() {
    let props = LzmaEncProps::new().with_lclppb(4, 1, 2);
    assert!(Lzma2Encoder::new(&props).is_err());
    assert!(Lzma2Encoder::new(&LzmaEncProps::new().with_lclppb(4, 0, 2)).is_ok());
}

#[test]
fn lzma2_matches_the_reference_encoder() {
    let Some(oracle) = tool("lzma2-oracle") else {
        return;
    };
    let dir = tempdir("lzma2-parity");
    let src_path = dir.join("in.bin");
    let ref_path = dir.join("ref.lzma2");

    let settings = settings();
    let mut compared = 0usize;
    for (name, src) in corpus() {
        std::fs::write(&src_path, &src).unwrap();
        for props in &settings {
            let status = Command::new(&oracle)
                .args(oracle_args(props))
                .arg(&src_path)
                .arg(&ref_path)
                .status()
                .expect("run the reference LZMA2 encoder");
            assert!(status.success(), "reference failed on {name}");

            let mut enc = Lzma2Encoder::new(props).expect("encoder");
            enc.set_data_size(src.len() as u64);
            let mut got = vec![enc.properties()];
            let mut input = lzma_turbo::SliceStream::new(&src);
            enc.encode(&mut input, &mut got).expect("encode");

            let expected = std::fs::read(&ref_path).unwrap();
            assert!(
                expected == got,
                "not bit-exact on {name}: reference {} bytes, ours {} bytes",
                expected.len(),
                got.len()
            );
            compared += 1;
        }
    }
    assert!(compared > 0);
    eprintln!("compared {compared} LZMA2 streams against the reference");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Lzma2Writer` holds its input up to the dictionary and streams past it,
/// and either way writes what a one-shot encode of the whole input writes:
/// past the dictionary the data size no longer reaches the bytes. The
/// two-thread match finder takes the streamed input across threads too.
#[test]
fn lzma2_writer_streams_the_one_shot_bytes() {
    let base = LzmaEncProps::new().with_level(1).with_dict_size(1 << 18);
    let dict = base.normalized().dict_size as usize;
    let data = mixed(0x5eed_0006, (3 << 20).min(max_len()));
    let bt_mt = LzmaEncProps::new()
        .with_level(5)
        .with_dict_size(1 << 18)
        .with_num_threads(2);
    for (props, what) in [(base, "hc4"), (bt_mt, "bt4, two finder threads")] {
        for len in [0, 1000, dict, dict + 1, data.len()] {
            let len = len.min(data.len());
            let mut w = Lzma2Writer::new(Vec::new(), &props).expect("writer");
            for chunk in data[..len].chunks(65_521) {
                w.write_all(chunk).expect("write");
            }
            let streamed = w.finish().expect("finish");
            let want = Lzma2Encoder::new(&props)
                .expect("encoder")
                .encode_to_vec(&data[..len])
                .expect("encode");
            assert!(streamed == want, "{what}, {len} bytes");
        }
    }
}

/// A sink that, while armed, refuses every third write, and takes only part
/// of the others.
struct Flaky {
    out: Vec<u8>,
    calls: usize,
    armed: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.armed.get() && self.calls % 3 == 0 {
            return Err(std::io::Error::other("refused"));
        }
        let n = buf.len().min(4000);
        self.out.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `Write` says an error means none of the buffer was written, so a caller
/// may retry the same bytes. The writer used to take them into the encoder
/// and only then fail to pass the output on, so the retry compressed them
/// twice; and a refused write lost the output it was carrying.
///
/// The sink is refused only while input goes in, and some output is certain
/// to come back then whatever the threads' timing: the input channel holds
/// two chunks, so the last writes wait on the encoder having read, and so
/// having written, most of four megabytes.
#[test]
fn lzma2_writer_takes_nothing_from_a_write_that_fails() {
    let props = LzmaEncProps::new().with_level(1).with_dict_size(1 << 16);
    let data = mixed(0x5eed_0007, (4 << 20).min(max_len()));
    let armed = std::rc::Rc::new(std::cell::Cell::new(true));
    let mut w = Lzma2Writer::new(
        Flaky {
            out: Vec::new(),
            calls: 0,
            armed: std::rc::Rc::clone(&armed),
        },
        &props,
    )
    .expect("writer");
    let mut refused = 0;
    for chunk in data.chunks(10_000) {
        loop {
            match w.write(chunk) {
                Ok(n) => {
                    assert_eq!(n, chunk.len(), "a short write");
                    break;
                }
                Err(_) => refused += 1,
            }
        }
    }
    while w.flush().is_err() {
        refused += 1;
    }
    armed.set(false);
    let prop = w.properties();
    let sink = w.finish().expect("finish");
    assert!(refused > 0, "the sink never refused a write");
    let mut back = Vec::new();
    Lzma2Reader::new(std::io::Cursor::new(&sink.out), prop)
        .expect("reader")
        .read_to_end(&mut back)
        .expect("decode");
    assert!(back == data, "the retried writes changed the stream");
}
