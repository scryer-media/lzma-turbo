//! The public run scanner must find exactly the boundaries the streams were
//! built with, and must not care how the bytes are handed to it.

mod common;

use lzma_turbo::{Error, Lzma2Run, Lzma2RunChunks, Lzma2RunScanner};

use common::{copy_run, join_runs, multi_run, pseudo_random, xz_run};

/// Scans a whole stream, feeding it `chunk` bytes at a time.
fn scan(data: &[u8], chunk: usize) -> Result<Vec<Lzma2Run>, Error> {
    let mut s = Lzma2RunScanner::new();
    let mut runs = Vec::new();
    let mut pos = 0;
    while pos < data.len() && !s.finished() {
        let end = (pos + chunk).min(data.len());
        pos += s.feed(&data[pos..end])?;
        while let Some(r) = s.next_run() {
            runs.push(r);
        }
    }
    while let Some(r) = s.next_run() {
        runs.push(r);
    }
    assert!(s.finished(), "scanner did not reach the end marker");
    Ok(runs)
}

#[test]
fn finds_every_run_in_a_compressed_stream() {
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "zeros.p1.xz"];
    let (_, packed, plain) = multi_run(&names, 3);

    let runs = scan(&packed, usize::MAX).expect("scan");
    assert_eq!(runs.len(), names.len() * 3);

    let mut in_pos = 0u64;
    let mut out_pos = 0u64;
    for r in &runs {
        assert!(r.has_dict_reset);
        assert_eq!(r.in_offset, in_pos);
        assert_eq!(r.out_offset, out_pos);
        in_pos += r.packed_len;
        out_pos += r.unpacked_len;
    }
    // Everything but the one-byte end marker is accounted for.
    assert_eq!(in_pos, packed.len() as u64 - 1);
    assert_eq!(out_pos, plain.len() as u64);
}

#[test]
fn a_split_header_is_carried_over_not_re_read() {
    let (_, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);
    let whole = scan(&packed, usize::MAX).expect("scan");
    // One byte at a time splits every multi-byte chunk header there is.
    for chunk in [1usize, 2, 3, 5, 7, 13, 64, 1021] {
        assert_eq!(scan(&packed, chunk).expect("scan"), whole, "chunk={chunk}");
    }
}

#[test]
fn uncompressed_chunks_delimit_runs_too() {
    // 0x01 resets the dictionary and so starts a run; 0x02 does not.
    let runs = [
        copy_run(&pseudo_random(200_000, 1)),
        copy_run(&pseudo_random(3, 2)),
        copy_run(&pseudo_random(70_000, 3)),
    ];
    let (packed, plain) = join_runs(&runs);
    let found = scan(&packed, 3).expect("scan");
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].unpacked_len, 200_000);
    assert_eq!(found[1].unpacked_len, 3);
    assert_eq!(found[2].unpacked_len, 70_000);
    assert_eq!(
        found.iter().map(|r| r.unpacked_len).sum::<u64>(),
        plain.len() as u64
    );
}

#[test]
fn a_compressed_run_followed_by_a_copy_run_sizes_both() {
    // Regression: the chunk-header state machine ORs into `unpackSize`, so a
    // walk that skips a compressed payload must zero it the way the decoder's
    // own countdown does, or the next copy chunk inherits its high bits.
    let (_, lzma) = xz_run("text.p1.xz");
    let plain_len = lzma.plain.len() as u64;
    let copy = copy_run(&pseudo_random(1000, 9));
    let (packed, _) = join_runs(&[lzma, copy]);
    let found = scan(&packed, usize::MAX).expect("scan");
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].unpacked_len, plain_len);
    assert_eq!(found[1].unpacked_len, 1000);
}

#[test]
fn a_stream_that_never_resets_the_dictionary_is_one_run() {
    // What `7zz -mmt=1` produces: one reset at the start and nothing after it.
    // There is no second boundary to cut at, so there is nothing to decode in
    // parallel, and the multi-threaded decoder has to fall back to the
    // single-threaded one for the whole stream.
    let (_, packed, _) = multi_run(&["text.p1.xz"], 1);
    let found = scan(&packed, 7).expect("scan");
    assert_eq!(found.len(), 1);
    assert!(found[0].has_dict_reset);

    // The format has no way to express a run without one: `needInitLevel`
    // starts at 0xE0, so the first chunk must reset the dictionary.
    let mut broken = packed.clone();
    broken[0] &= 0xDF;
    let mut s = Lzma2RunScanner::new();
    assert_eq!(s.feed(&broken), Err(Error::CorruptData));
}

#[test]
fn a_bad_control_byte_is_an_error() {
    let mut s = Lzma2RunScanner::new();
    // 0x7F is neither a copy chunk (<= 2) nor an LZMA chunk (>= 0x80).
    assert_eq!(s.feed(&[0x7F]), Err(Error::CorruptData));
    // And the error sticks.
    assert_eq!(s.feed(&[0xE0]), Err(Error::CorruptData));
}

#[test]
fn an_lzma_chunk_before_any_dictionary_reset_is_an_error() {
    let mut s = Lzma2RunScanner::new();
    assert_eq!(s.feed(&[0x80]), Err(Error::CorruptData));
}

#[test]
fn nothing_past_the_end_marker_is_consumed() {
    let (_, r) = xz_run("tiny.p1.xz");
    let (mut packed, _) = join_runs(&[r]);
    let len = packed.len();
    packed.extend_from_slice(b"trailing garbage");
    let mut s = Lzma2RunScanner::new();
    assert_eq!(s.feed(&packed), Ok(len));
    assert!(s.finished());
    assert_eq!(s.in_position(), len as u64);
}

/// Appends `0x02` chunks (stored, no reset) holding `plain` to a run, so the
/// run goes on without a dictionary reset.
fn append_copy_chunks(packed: &mut Vec<u8>, plain: &[u8]) {
    for piece in plain.chunks(1 << 16) {
        packed.push(0x02);
        packed.extend_from_slice(&((piece.len() - 1) as u16).to_be_bytes());
        packed.extend_from_slice(piece);
    }
}

fn assert_chunks_cover_the_run(r: &Lzma2Run) {
    let c = r.chunks;
    assert_eq!(c.lzma_packed + c.copy_packed, r.packed_len, "{r:?}");
    assert_eq!(c.lzma_unpacked + c.copy_unpacked, r.unpacked_len, "{r:?}");
    assert!(c.count() > 0);
}

#[test]
fn a_run_record_accounts_for_every_byte_by_chunk_kind() {
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "zeros.p1.xz"];
    let (_, packed, _) = multi_run(&names, 2);
    let whole = scan(&packed, usize::MAX).expect("scan");
    for r in &whole {
        assert_chunks_cover_the_run(r);
        // An LZMA-coded run starts with a chunk that resets the dictionary,
        // which is also a state reset with new properties; xz stores the
        // random fixture, and a stored run has neither.
        if r.chunks.lzma_chunks > 0 {
            assert!(r.chunks.state_resets >= 1 && r.chunks.prop_resets >= 1);
        } else {
            assert_eq!((r.chunks.state_resets, r.chunks.prop_resets), (0, 0));
        }
    }
    // How the bytes arrive changes nothing in the record.
    for chunk in [1usize, 3, 6, 4099] {
        assert_eq!(scan(&packed, chunk).expect("scan"), whole, "chunk={chunk}");
    }
}

#[test]
fn a_stored_run_is_all_copy_chunks() {
    let plain = pseudo_random(200_000, 5);
    let (packed, _) = join_runs(&[copy_run(&plain)]);
    let found = scan(&packed, 5).expect("scan");
    assert_eq!(found.len(), 1);
    assert_chunks_cover_the_run(&found[0]);
    assert_eq!(
        found[0].chunks,
        Lzma2RunChunks {
            copy_chunks: 4,
            copy_packed: 200_000 + 4 * 3,
            copy_unpacked: 200_000,
            ..Lzma2RunChunks::default()
        }
    );
}

#[test]
fn an_lzma_run_that_goes_on_in_stored_chunks_is_split_exactly() {
    let (_, lzma) = xz_run("text.p1.xz");
    let lzma_packed = lzma.packed.len() as u64;
    let lzma_unpacked = lzma.plain.len() as u64;
    let mut run = lzma;
    let tail = pseudo_random(150_000, 11);
    append_copy_chunks(&mut run.packed, &tail);
    let (packed, _) = join_runs(&[run, copy_run(&pseudo_random(10, 3))]);
    let found = scan(&packed, 7).expect("scan");
    assert_eq!(found.len(), 2);
    let c = found[0].chunks;
    assert_chunks_cover_the_run(&found[0]);
    assert_eq!(c.lzma_packed, lzma_packed);
    assert_eq!(c.lzma_unpacked, lzma_unpacked);
    assert_eq!(c.copy_chunks, 3);
    assert_eq!(c.copy_unpacked, 150_000);
    assert_eq!(c.copy_packed, 150_000 + 3 * 3);
    // The stored run after it starts with `0x01` and is all copy.
    assert_eq!(found[1].chunks.lzma_chunks, 0);
    assert_eq!(found[1].chunks.copy_unpacked, 10);
}
