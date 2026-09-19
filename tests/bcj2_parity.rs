//! Bit-exactness of the BCJ2 converter against the reference SDK.
//!
//! `cargo xtask lzma-util` builds `bcj2-oracle` from the pinned SDK's own
//! `Bcj2.c` and `Bcj2Enc.c` into `target/lzma-util`. Both directions are
//! compared against it byte for byte. As with the other parity tests, a
//! missing binary is a skip unless `LZMA_TURBO_LZMA_UTIL_REQUIRE` is set.
//!
//! BCJ2 only does anything to bytes that look like x86 branches, so the corpus
//! is the shared one plus generated code-like data plus one case of real
//! machine code: the oracle binary itself, which is whatever the host builds.

#![cfg(all(feature = "std", feature = "filters"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use lzma_turbo::filters::bcj2::{
    Bcj2Dec, Bcj2DecStreams, Bcj2Enc, Bcj2EncFinishMode, Bcj2EncOut, Bcj2Streams, NUM_STREAMS,
    STREAM_CALL, STREAM_JUMP, STREAM_MAIN, STREAM_RC, encode_to_streams_with,
};

mod corpus;

use corpus::{corpus, tempdir, tool};

// ---------------------------------------------------------------------------
// Inputs.
// ---------------------------------------------------------------------------

/// A xorshift, so the inputs are generated rather than committed.
struct Rng(u32);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// Bytes shaped like x86 code: `E8`, `E9` and `0F 8x` at every alignment, with
/// offsets on both sides of the encoder's relative limit, and runs of the same
/// opcode so that markers overlap each other's four-byte offsets.
fn code_like(seed: u32, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        let r = rng.next_u32();
        match r % 8 {
            0 => {
                out.push(0xE8);
                out.extend_from_slice(&(rng.next_u32() & 0x000F_FFFF).to_le_bytes());
            }
            1 => {
                out.push(0xE9);
                // A full-width offset, which is past the default limit.
                out.extend_from_slice(&rng.next_u32().to_le_bytes());
            }
            2 => {
                out.push(0x0F);
                out.push(0x80 | (rng.next_u32() & 0x0F) as u8);
                out.extend_from_slice(&(rng.next_u32() & 0x0000_FFFF).to_le_bytes());
            }
            3 => out.extend_from_slice(&r.to_le_bytes()),
            4 => out.push(0xE8),
            5 => out.push(0xE9),
            6 => out.extend_from_slice(&[0x0F, 0x8F]),
            _ => out.push((r >> 11) as u8),
        }
    }
    out.truncate(len);
    out
}

/// Every one-byte and two-byte marker at every offset inside a word, for the
/// short lengths the encoder's lookahead and the decoder's `temp` turn on.
fn short_cases() -> Vec<(String, Vec<u8>)> {
    let mut cases = Vec::new();
    for len in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        cases.push((format!("zeros-{len}"), vec![0u8; len]));
        cases.push((format!("e8-{len}"), vec![0xE8u8; len]));
        cases.push((format!("code-{len}"), code_like(0x9E37_79B9, len)));
        let mut mixed = vec![0x90u8; len];
        if len >= 2 {
            mixed[len - 2] = 0x0F;
            mixed[len - 1] = 0x85;
        }
        cases.push((format!("tail-marker-{len}"), mixed));
    }
    // The encoder keeps up to four bytes back, and the decoder's `temp` holds
    // up to four: sit either side of a marker that straddles that.
    for pad in 0..6usize {
        let mut buf = vec![0x90u8; pad];
        buf.extend_from_slice(&[0xE8, 0x01, 0x02, 0x03, 0x04, 0x90, 0x90]);
        cases.push((format!("marker-at-{pad}"), buf));
    }
    cases
}

fn cases(bin: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all = short_cases();
    all.extend(corpus());
    for (tag, len) in [
        ("code-small", 1024usize),
        ("code-chunk-under", (1 << 16) - 1),
        ("code-chunk-over", (1 << 16) + 1),
        ("code-big", 200_003),
    ] {
        all.push((tag.to_owned(), code_like(0x5DEE_CE66 ^ len as u32, len)));
    }
    // Real machine code for the host this runs on, without committing any.
    if let Ok(bytes) = std::fs::read(bin) {
        all.push(("oracle-binary".to_owned(), bytes));
    }
    all
}

// ---------------------------------------------------------------------------
// The oracle.
// ---------------------------------------------------------------------------

struct Oracle {
    bin: PathBuf,
    dir: PathBuf,
}

impl Oracle {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// C: `Bcj2Enc_Encode` with `BCJ2_ENC_FINISH_MODE_END_STREAM`.
    fn encode(&self, data: &[u8], relat_limit: u32, file_size: Option<u64>) -> Bcj2Streams {
        let src = self.path("in.bin");
        std::fs::write(&src, data).unwrap();
        let names = ["main.bin", "call.bin", "jump.bin", "rc.bin"];
        let outs: Vec<PathBuf> = names.iter().map(|n| self.path(n)).collect();
        for o in &outs {
            let _ = std::fs::remove_file(o);
        }
        let size = file_size.map_or_else(|| "-".to_owned(), |n| n.to_string());
        let mut cmd = Command::new(&self.bin);
        cmd.args(["enc", &relat_limit.to_string(), &size])
            .arg(&src)
            .args(&outs);
        assert!(cmd.status().unwrap().success(), "bcj2-oracle enc failed");
        Bcj2Streams {
            main: std::fs::read(&outs[0]).unwrap(),
            call: std::fs::read(&outs[1]).unwrap(),
            jump: std::fs::read(&outs[2]).unwrap(),
            rc: std::fs::read(&outs[3]).unwrap(),
        }
    }

    /// C: `Bcj2Dec_Decode` in one call.
    fn decode(&self, s: &Bcj2Streams, orig: usize) -> Vec<u8> {
        let names = ["dmain.bin", "dcall.bin", "djump.bin", "drc.bin"];
        let ins: Vec<PathBuf> = names.iter().map(|n| self.path(n)).collect();
        for (path, bytes) in ins.iter().zip([&s.main, &s.call, &s.jump, &s.rc]) {
            std::fs::write(path, bytes).unwrap();
        }
        let out = self.path("dout.bin");
        let _ = std::fs::remove_file(&out);
        let mut cmd = Command::new(&self.bin);
        cmd.args(["dec", &orig.to_string()]).args(&ins).arg(&out);
        assert!(cmd.status().unwrap().success(), "bcj2-oracle dec failed");
        std::fs::read(&out).unwrap()
    }
}

fn oracle(tag: &str) -> Option<Oracle> {
    let bin = tool("bcj2-oracle")?;
    Some(Oracle {
        dir: tempdir(tag),
        bin,
    })
}

// ---------------------------------------------------------------------------
// This crate's side.
// ---------------------------------------------------------------------------

/// The whole buffer in one call, with the settings the oracle was given.
fn ours(data: &[u8], relat_limit: u32, file_size: Option<u64>) -> Bcj2Streams {
    let mut enc = Bcj2Enc::new();
    enc.set_relat_limit(relat_limit);
    if let Some(n) = file_size {
        enc.set_file_size(n);
    }
    encode_to_streams_with(&mut enc, data)
}

/// The same, fed `chunk` source bytes at a time into output windows that are
/// themselves too small, so that every "this stream is full" exit is taken.
fn ours_in_pieces(data: &[u8], chunk: usize, window: usize) -> Bcj2Streams {
    let mut enc = Bcj2Enc::new();
    let mut bufs: [Vec<u8>; NUM_STREAMS] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut pos = [0usize; NUM_STREAMS];
    let mut at = 0usize;
    loop {
        let end = (at + chunk).min(data.len());
        let last = end == data.len();
        enc.set_finish_mode(if last {
            Bcj2EncFinishMode::EndStream
        } else {
            Bcj2EncFinishMode::Continue
        });
        let piece = &data[at..end];
        let mut piece_pos = 0usize;
        loop {
            // Small windows, so that every "this stream is full" exit is
            // taken and the encoder has to be resumed.
            for b in &mut bufs {
                if b.is_empty() {
                    b.resize(window.max(4), 0);
                }
            }
            let [m, c, j, r] = &mut bufs;
            let mut out = Bcj2EncOut {
                bufs: [
                    m.as_mut_slice(),
                    c.as_mut_slice(),
                    j.as_mut_slice(),
                    r.as_mut_slice(),
                ],
                pos,
            };
            enc.encode(&mut out, piece, &mut piece_pos);
            pos = out.pos;
            let Some(full) = enc.full_stream() else {
                break;
            };
            let grow = window.max(4);
            bufs[full].resize(bufs[full].len() + grow, 0);
        }
        at = end;
        if last {
            break;
        }
    }
    assert!(enc.is_finished(), "chunk {chunk} window {window}");
    let [mut main, mut call, mut jump, mut rc] = bufs;
    main.truncate(pos[STREAM_MAIN]);
    call.truncate(pos[STREAM_CALL]);
    jump.truncate(pos[STREAM_JUMP]);
    rc.truncate(pos[STREAM_RC]);
    Bcj2Streams {
        main,
        call,
        jump,
        rc,
    }
}

/// Our decoder over complete streams, in one call.
fn decode_whole(s: &Bcj2Streams, orig: usize) -> Vec<u8> {
    let mut out = vec![0u8; orig];
    let mut dec = Bcj2Dec::new();
    let mut st = Bcj2DecStreams::new(&s.main, &s.call, &s.jump, &s.rc, &mut out);
    dec.decode(&mut st).expect("decode");
    assert_eq!(st.dest_pos, orig, "decoder produced the wrong size");
    out
}

/// Our decoder fed `sizes` bytes of each stream at a time.
fn decode_in_pieces(s: &Bcj2Streams, orig: usize, mut next: impl FnMut(usize) -> usize) -> Vec<u8> {
    let mut out = vec![0u8; orig];
    let mut dec = Bcj2Dec::new();
    let mut at = [0usize; NUM_STREAMS];
    let lens = [s.main.len(), s.call.len(), s.jump.len(), s.rc.len()];
    let bufs = [&s.main, &s.call, &s.jump, &s.rc];
    let mut done = 0usize;
    let mut stuck = 0usize;
    while done < orig {
        let mut end = [0usize; NUM_STREAMS];
        for i in 0..NUM_STREAMS {
            let mut take = next(i);
            if i == STREAM_CALL || i == STREAM_JUMP {
                // Those two move four bytes at a time.
                take = take.next_multiple_of(4);
            }
            end[i] = (at[i] + take).min(lens[i]);
        }
        let dend = (done + next(NUM_STREAMS)).max(done + 1).min(orig);
        let mut st = Bcj2DecStreams::new(
            &bufs[0][at[0]..end[0]],
            &bufs[1][at[1]..end[1]],
            &bufs[2][at[2]..end[2]],
            &bufs[3][at[3]..end[3]],
            &mut out[done..dend],
        );
        dec.decode(&mut st).expect("decode");
        let before = done;
        for i in 0..NUM_STREAMS {
            at[i] = end[i] - st.bufs[i].len();
        }
        done += st.dest_pos;
        assert!(
            done != before || at.iter().zip(lens).any(|(a, l)| *a < l),
            "the decoder made no progress and has everything"
        );
        stuck += 1;
        assert!(stuck <= 4 * orig + 64, "the decoder is not converging");
    }
    out
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// Every setting the encoder's conversion conditions branch on: the default
/// limit, one that disables conversion, the maximum, and a file size that
/// pushes targets out of range.
const SETTINGS: [(u32, Option<u64>); 5] = [
    (0x0F << 24, None),
    (0, None),
    (1 << 31, None),
    (0x0F << 24, Some(64)),
    (0x0F << 24, Some(1 << 20)),
];

#[test]
fn the_encoder_matches_the_sdk_byte_for_byte() {
    let Some(o) = oracle("bcj2-enc") else {
        return;
    };
    for (tag, data) in cases(&o.bin) {
        for (limit, size) in SETTINGS {
            let want = o.encode(&data, limit, size);
            let got = ours(&data, limit, size);
            assert_eq!(
                got.main, want.main,
                "{tag} main, limit {limit} size {size:?}"
            );
            assert_eq!(
                got.call, want.call,
                "{tag} call, limit {limit} size {size:?}"
            );
            assert_eq!(
                got.jump, want.jump,
                "{tag} jump, limit {limit} size {size:?}"
            );
            assert_eq!(got.rc, want.rc, "{tag} rc, limit {limit} size {size:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&o.dir);
}

#[test]
fn the_decoder_undoes_the_reference_encoder() {
    let Some(o) = oracle("bcj2-dec") else {
        return;
    };
    for (tag, data) in cases(&o.bin) {
        for (limit, size) in SETTINGS {
            let want = o.encode(&data, limit, size);
            assert_eq!(decode_whole(&want, data.len()), data, "{tag} limit {limit}");
        }
    }
    let _ = std::fs::remove_dir_all(&o.dir);
}

#[test]
fn the_reference_decoder_undoes_this_encoder() {
    let Some(o) = oracle("bcj2-round") else {
        return;
    };
    for (tag, data) in cases(&o.bin) {
        let got = ours(&data, 0x0F << 24, None);
        assert_eq!(o.decode(&got, data.len()), data, "{tag}");
    }
    let _ = std::fs::remove_dir_all(&o.dir);
}

#[test]
fn encoding_in_pieces_is_bit_exact_with_the_sdk_too() {
    let Some(o) = oracle("bcj2-enc-pieces") else {
        return;
    };
    let data = code_like(0xC0FF_EE01, 40_009);
    let want = o.encode(&data, 0x0F << 24, None);
    for (chunk, window) in [
        (1usize, 1usize),
        (3, 4),
        (5, 7),
        (7, 1),
        (64, 16),
        (4096, 997),
    ] {
        let got = ours_in_pieces(&data, chunk, window);
        assert_eq!(got.main, want.main, "chunk {chunk} window {window} main");
        assert_eq!(got.call, want.call, "chunk {chunk} window {window} call");
        assert_eq!(got.jump, want.jump, "chunk {chunk} window {window} jump");
        assert_eq!(got.rc, want.rc, "chunk {chunk} window {window} rc");
    }
    let _ = std::fs::remove_dir_all(&o.dir);
}

#[test]
fn decoding_in_pieces_gives_what_decoding_whole_gives() {
    let Some(o) = oracle("bcj2-dec-pieces") else {
        return;
    };
    let data = code_like(0xC0FF_EE02, 40_009);
    let s = o.encode(&data, 0x0F << 24, None);
    assert_eq!(decode_whole(&s, data.len()), data);

    for fixed in [1usize, 7] {
        assert_eq!(
            decode_in_pieces(&s, data.len(), |_| fixed),
            data,
            "{fixed}-byte pieces"
        );
    }
    // And random-sized pieces per stream, from one seed so the run is the same
    // on every machine.
    let mut rng = Rng(0x1234_5678);
    assert_eq!(
        decode_in_pieces(&s, data.len(), move |_| (rng.next_u32() % 23) as usize),
        data,
        "random pieces"
    );
    let _ = std::fs::remove_dir_all(&o.dir);
}
