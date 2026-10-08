//! The threaded match finder on inputs too big for the oracle sweep.
//!
//! `tests/lzma_parity.rs` and `tests/lzma2_mt_parity.rs` pin the bytes against
//! the SDK built without `Z7_ST`. This file needs no reference binary: it
//! covers the paths a few hundred kilobytes never reach — `MatchFinder_MoveBlock`
//! running from the hash thread under both critical sections, the `hashBuf`
//! ring wrapping many times, and `MatchFinder_Normalize3` on the high hash —
//! and checks the things that must hold whatever the finder does: the stream
//! decodes back, and the same input gives the same bytes every time and
//! through every entry point.

#![cfg(all(feature = "std", feature = "enc"))]

use std::io::Read as _;

use lzma_turbo::{
    Error, Lzma2Encoder, Lzma2Reader, LzmaEncProps, MatchFinderKind, SeqInStream, SliceStream,
};

#[path = "corpus/mod.rs"]
mod shared;

/// Runs and short repeated phrases, which give the binary tree long chains to
/// walk and so keep the bt thread behind the hash thread.
///
/// `LZMA_TURBO_CORPUS_MAX` caps the length; see [`shared::max_len`]. Under
/// that cap this file's big inputs no longer reach `MatchFinder_MoveBlock` at
/// every dictionary, which is the trade memcheck asks for: that lane is there
/// to watch the three threads touch memory, and the uncapped run everywhere
/// else is what covers the sliding window.
fn corpus(len: usize) -> Vec<u8> {
    let len = len.min(shared::max_len());
    let mut v = Vec::with_capacity(len);
    let mut x = 0x1234_5678u32;
    while v.len() < len {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let n = (x >> 16) as usize % 40 + 1;
        let b = (x >> 8) as u8 % 7;
        for _ in 0..n.min(len - v.len()) {
            v.push(b'a' + b);
        }
    }
    v
}

fn mt(props: &LzmaEncProps) -> LzmaEncProps {
    props.with_num_threads(2)
}

fn encode_mem(props: &LzmaEncProps, src: &[u8]) -> (u8, Vec<u8>) {
    let mut enc = Lzma2Encoder::new(props).expect("encoder");
    let out = enc.encode_to_vec(src).expect("encode");
    (enc.properties(), out)
}

fn encode_stream(props: &LzmaEncProps, src: &[u8]) -> (u8, Vec<u8>) {
    let mut enc = Lzma2Encoder::new(props).expect("encoder");
    let mut out = Vec::new();
    enc.encode_send(&mut SliceStream::new(src), &mut out)
        .expect("encode");
    (enc.properties(), out)
}

fn round_trip(prop: u8, encoded: &[u8], src: &[u8], what: &str) {
    let mut reader = Lzma2Reader::new(std::io::Cursor::new(encoded), prop).expect("reader");
    let mut out = Vec::new();
    reader.read_to_end(&mut out).expect("decode");
    assert!(out == src, "round trip lost bytes on {what}");
}

/// Every match finder, at dictionaries smaller than the input, which is what
/// makes the window slide and the hash normalize.
#[test]
fn the_threaded_finder_round_trips_at_every_match_finder() {
    let src = corpus(3_000_000);
    for kind in [
        MatchFinderKind::Bt2,
        MatchFinderKind::Bt3,
        MatchFinderKind::Bt4,
        MatchFinderKind::Bt5,
        MatchFinderKind::Hc4,
        MatchFinderKind::Hc5,
    ] {
        for dict in [1u32 << 16, 1 << 20, 1 << 25] {
            let props = mt(&LzmaEncProps::new()
                .with_level(9)
                .with_match_finder(kind)
                .with_dict_size(dict));
            let (prop, out) = encode_mem(&props, &src);
            round_trip(prop, &out, &src, &format!("{kind:?} dict {dict}"));
        }
    }
}

/// Short inputs, where the stream ends before the first `hashBuf` block is
/// full and the bt thread takes the "stream was finished" path.
#[test]
fn the_threaded_finder_round_trips_at_the_short_lengths() {
    for len in [0usize, 1, 2, 4, 5, 6, 100, 5000] {
        let src = corpus(len);
        for level in [1u32, 5, 9] {
            let props = mt(&LzmaEncProps::new()
                .with_level(level)
                .with_dict_size(1 << 16));
            let (prop, out) = encode_mem(&props, &src);
            round_trip(prop, &out, &src, &format!("len {len} level {level}"));
        }
    }
}

/// The threads must not leave anything behind: the same input gives the same
/// bytes on every run, and from memory or from a stream.
#[test]
fn the_threaded_finder_is_deterministic() {
    let src = corpus(1_500_000);
    for dict in [1u32 << 16, 1 << 22] {
        let props = mt(&LzmaEncProps::new().with_level(6).with_dict_size(dict));
        let first = encode_mem(&props, &src);
        for _ in 0..3 {
            assert!(
                encode_mem(&props, &src) == first,
                "dict {dict}: run differs"
            );
        }
        assert!(
            encode_stream(&props, &src) == first,
            "dict {dict}: the streaming entry point differs from the memory one"
        );
    }
}

/// A dictionary past `0xFFFFFF`, which is what sets `MFB.bigHash` and swaps
/// `GetHeads5` for `GetHeads5b`.
#[test]
fn the_big_hash_heads_round_trip() {
    let src = corpus(3_000_000);
    for dict in [1u32 << 24, 1 << 26] {
        let props = mt(&LzmaEncProps::new()
            .with_level(9)
            .with_match_finder(MatchFinderKind::Bt5)
            .with_dict_size(dict));
        let (prop, out) = encode_stream(&props, &src);
        round_trip(prop, &out, &src, &format!("big hash dict {dict}"));
    }
}

/// Pseudo-random bytes with back-references sprinkled through them: mostly
/// incompressible, so the bt thread keeps pace with the hash thread instead of
/// lagging a long chain behind it, with enough matches for the tree to be
/// walked. That pacing is what the sliding-window reproduction below needs.
fn mixed(len: usize) -> Vec<u8> {
    let len = len.min(shared::max_len());
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if (x >> 60) < 4 {
            let n = ((x >> 8) as usize % 64 + 4).min(len - v.len());
            let back = (x >> 20) as usize % 100_000 + 1;
            if back < v.len() {
                for _ in 0..n {
                    v.push(v[v.len() - back]);
                }
                continue;
            }
        }
        v.push(x as u8);
    }
    v
}

/// An input longer than the window, through `encode_send`, so that
/// `MatchFinder_MoveBlock` slides the window under the bt and lz threads.
///
/// This used to panic on the bt thread ("range end index ... out of range" in
/// `match_run`) and then hang: the move rewrote the bt thread's window index
/// while that thread held a `&mut` to it across `get_next_block`, so the
/// compiled bt thread carried on from its old index and ran off the end of the
/// window. 8 MiB dictionaries failed from about 15 MB of input, 1 MiB ones at
/// 16 MiB.
///
/// The output must decode back, and be the single-threaded finder's output
/// byte for byte: the port's threaded finder finds the same matches (see
/// `src/enc/lz_find_mt.rs`), so even the size tolerance a different match
/// choice would need is not called for.
#[test]
fn the_threaded_finder_survives_the_window_sliding_past_the_input() {
    for (dict, len) in [(8u32 << 20, 16_000_000usize), (1 << 20, 16 << 20)] {
        let src = mixed(len);
        let st = LzmaEncProps::new().with_level(6).with_dict_size(dict);
        let (prop, out) = encode_stream(&mt(&st), &src);
        round_trip(prop, &out, &src, &format!("dict {dict} len {len}"));

        let (_, single) = encode_stream(&st, &src);
        assert!(
            out == single,
            "dict {dict} len {len}: threaded finder {} bytes, single-threaded {}",
            out.len(),
            single.len()
        );
    }
}

/// A source that hands out `ok` bytes of `data` and then fails, either with an
/// error or by panicking.
struct FailingStream<'a> {
    data: &'a [u8],
    pos: usize,
    ok: usize,
    panic: bool,
}

impl SeqInStream for FailingStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if self.pos >= self.ok {
            if self.panic {
                panic!("injected panic in the input stream");
            }
            return Err(Error::Read);
        }
        let n = buf.len().min(self.ok - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A source that fails part way must make the threaded encode return an
/// error. The stream is read on the hash thread, so a panic there used to
/// leave the bt and lz threads waiting for blocks that never came; it now ends
/// the stream and the encoder's `CheckErrors` reports it.
#[test]
fn a_failing_input_returns_an_error_from_the_threaded_finder() {
    let src = mixed(3_000_000);
    let props = mt(&LzmaEncProps::new().with_level(6).with_dict_size(1 << 20));
    for panic in [false, true] {
        let mut enc = Lzma2Encoder::new(&props).expect("encoder");
        let mut out = Vec::new();
        let mut input = FailingStream {
            data: &src,
            pos: 0,
            ok: src.len().min(2_000_000),
            panic,
        };
        let r = enc.encode_send(&mut input, &mut out);
        assert!(
            r.is_err(),
            "panic {panic}: the failed read was not reported"
        );
    }
}
