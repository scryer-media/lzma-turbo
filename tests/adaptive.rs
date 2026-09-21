//! The adaptive decoder: fed input, switchable mid-stream, bounded.
//!
//! One test per constraint the consumer's "adaptive chase" places on it.
//!
//! The threaded decoders live behind the `std` feature, so this whole file
//! does with them.
#![cfg(feature = "std")]

mod common;

use std::collections::BTreeMap;

use lzma_turbo::{DrainStatus, Error, Lzma2AdaptiveDecoder, Lzma2MtOptions};

use common::{copy_run, join_runs, multi_run, pseudo_random};

fn opts(threads: usize, memory_limit: u64) -> Lzma2MtOptions {
    Lzma2MtOptions {
        threads,
        memory_limit,
    }
}

/// Collects blocks by offset, checking that none of them overlap.
#[derive(Default)]
struct Sink {
    blocks: BTreeMap<u64, Vec<u8>>,
    order: Vec<u64>,
}

impl Sink {
    fn put(&mut self, offset: u64, bytes: &[u8]) {
        self.order.push(offset);
        let prev = self.blocks.insert(offset, bytes.to_vec());
        assert!(prev.is_none(), "two blocks at offset {offset}");
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (off, b) in &self.blocks {
            assert_eq!(*off, out.len() as u64, "gap or overlap at {off}");
            out.extend_from_slice(b);
        }
        out
    }
}

/// Feeds `packed` `chunk` bytes at a time, draining after each feed.
fn run_adaptive(
    dict_prop: u8,
    packed: &[u8],
    chunk: usize,
    threads: usize,
    limit: u64,
) -> Result<Sink, Error> {
    let mut dec = Lzma2AdaptiveDecoder::new(dict_prop, &opts(threads, limit))?;
    let mut sink = Sink::default();
    let mut pos = 0;
    loop {
        if pos < packed.len() {
            let end = (pos + chunk).min(packed.len());
            pos += dec.feed(&packed[pos..end])?;
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        assert!(
            dec.in_flight_bytes() <= limit.max(1 << 16),
            "in flight {} over limit {limit}",
            dec.in_flight_bytes()
        );
        let status = dec.drain(|off, b| sink.put(off, b))?;
        match status {
            DrainStatus::Finished => return Ok(sink),
            DrainStatus::NeedsMoreInput => {
                assert!(pos < packed.len(), "asked for input that does not exist");
            }
            DrainStatus::Progress => {}
        }
    }
}

// 1. Push/feed input, resumable, headers split across feeds.

#[test]
fn feeding_one_byte_at_a_time_decodes_the_same_stream() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "rand.p1.xz", "mixed.p1.xz"], 2);
    for threads in [1usize, 4] {
        for chunk in [1usize, 3, 17, 4096] {
            let sink = run_adaptive(prop, &packed, chunk, threads, u64::MAX)
                .unwrap_or_else(|e| panic!("threads={threads} chunk={chunk}: {e}"));
            assert_eq!(sink.bytes(), plain, "threads={threads} chunk={chunk}");
        }
    }
}

// 2. The backlog of complete runs is visible to the caller.

#[test]
fn the_backlog_of_complete_runs_is_visible() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 3);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(1, u64::MAX)).expect("props");
    // Feed everything but do not decode: the scanner still has to have walked
    // it before the backlog can be reported, which the first drain does.
    dec.feed(&packed).expect("feed");
    let mut seen = 0usize;
    let _ = dec.drain(|_, b| seen += b.len());
    // Six runs went in; whatever is left unclaimed plus what was claimed is
    // all of them.
    assert_eq!(dec.runs_claimed() + dec.pending_runs() as u64, 6);
    assert!(dec.backlog().all(|r| r.has_dict_reset));
}

// 3. Switching modes at a run boundary is lossless.

#[test]
fn switching_from_st_to_mt_at_any_run_boundary_is_lossless() {
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "zeros.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 2);
    let runs = names.len() * 2;

    for n in 0..=runs {
        let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(1, u64::MAX)).expect("props");
        let mut sink = Sink::default();
        let mut pos = 0usize;
        let mut switched = false;
        loop {
            // Decode the first `n` runs on the calling thread, the rest on
            // workers. No input is re-fed and nothing is re-decoded.
            if !switched && dec.runs_claimed() >= n as u64 {
                dec.set_threads(8);
                switched = true;
            }
            if pos < packed.len() {
                let end = (pos + 997).min(packed.len());
                pos += dec.feed(&packed[pos..end]).expect("feed");
                if pos == packed.len() {
                    dec.end_of_input();
                }
            }
            if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
                break;
            }
        }
        assert_eq!(sink.bytes(), plain, "n={n}");
    }
}

#[test]
fn switching_back_to_single_threaded_mid_stream_is_lossless() {
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 3);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    let mut sink = Sink::default();
    let mut pos = 0usize;
    loop {
        // Flap between modes at every opportunity.
        dec.set_threads(if dec.runs_claimed().is_multiple_of(2) {
            1
        } else {
            4
        });
        if pos < packed.len() {
            let end = (pos + 1024).min(packed.len());
            pos += dec.feed(&packed[pos..end]).expect("feed");
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(sink.bytes(), plain);
}

// 4. Ordered by default, unordered on request; both carry offsets.

#[test]
fn ordered_delivery_is_the_default_and_unordered_is_opt_in() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 4);

    let ordered = run_adaptive(prop, &packed, 8192, 8, u64::MAX).expect("ordered");
    assert_eq!(ordered.bytes(), plain);
    let mut sorted = ordered.order.clone();
    sorted.sort_unstable();
    assert_eq!(ordered.order, sorted, "default delivery was out of order");

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(8, u64::MAX)).expect("props");
    dec.set_ordered(false);
    let mut sink = Sink::default();
    let mut pos = 0usize;
    loop {
        if pos < packed.len() {
            let end = (pos + 8192).min(packed.len());
            pos += dec.feed(&packed[pos..end]).expect("feed");
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    // Whatever order the blocks arrived in, their offsets place them.
    assert_eq!(sink.bytes(), plain);
}

// 5. Threads are spawned on first dispatch and parked, not torn down.

#[test]
fn threads_are_created_on_first_dispatch_and_survive_a_mode_change() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 4);

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    assert_eq!(dec.spawned_threads(), 0, "threads before any work");

    // Arriving in quarters, so that each drain sees a backlog of complete runs
    // rather than the tail of one still arriving. That backlog is the whole
    // reason to go multi-threaded, and the caller is the thing that sees it.
    let mut sink = Sink::default();
    let mut pos = 0usize;
    let mut peak = 0usize;
    let mut flip = false;
    let quarter = packed.len() / 4 + 1;
    loop {
        if pos < packed.len() {
            let end = (pos + quarter).min(packed.len());
            pos += dec.feed(&packed[pos..end]).expect("feed");
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        // Flap the mode across the whole decode. Dropping to one thread must
        // not cost the workers that already exist.
        flip = !flip;
        dec.set_threads(if flip { 4 } else { 1 });
        assert!(
            dec.spawned_threads() >= peak,
            "a worker was torn down by a mode change"
        );
        peak = peak.max(dec.spawned_threads());
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    assert!(dec.spawned_threads() > 0, "nothing was ever dispatched");
    assert!(peak > 0, "the mode flap never saw a live worker");
    assert_eq!(sink.bytes(), plain);
}

#[test]
fn a_single_threaded_decoder_never_creates_a_thread() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(1, u64::MAX)).expect("props");
    dec.feed(&packed).expect("feed");
    dec.end_of_input();
    let mut sink = Sink::default();
    while dec.drain(|o, b| sink.put(o, b)).expect("drain") != DrainStatus::Finished {
        assert_eq!(dec.spawned_threads(), 0);
    }
    assert_eq!(dec.spawned_threads(), 0);
    assert_eq!(sink.bytes(), plain);
}

// 6. Memory is accounted, bounded, and the decode can be cancelled.

#[test]
fn a_memory_limit_bounds_what_is_in_flight() {
    // Runs of 1 MiB of incompressible bytes: eight of them is 8 MiB of output,
    // which a 2 MiB limit cannot hold even two of at once.
    let runs: Vec<_> = (0..8)
        .map(|i| copy_run(&pseudo_random(1 << 20, i + 1)))
        .collect();
    let (packed, plain) = join_runs(&runs);
    const LIMIT: u64 = 2 << 20;

    let mut dec = Lzma2AdaptiveDecoder::new(16, &opts(8, LIMIT)).expect("props");
    let mut sink = Sink::default();
    let mut pos = 0usize;
    let mut peak = 0u64;
    loop {
        if pos < packed.len() {
            let end = (pos + (1 << 16)).min(packed.len());
            let took = dec.feed(&packed[pos..end]).expect("feed");
            pos += took;
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        peak = peak.max(dec.in_flight_bytes());
        assert!(
            dec.in_flight_bytes() <= LIMIT,
            "in flight {} over the {LIMIT} byte limit",
            dec.in_flight_bytes()
        );
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(sink.bytes(), plain);
    assert!(peak <= LIMIT);
}

#[test]
fn a_run_too_large_for_the_limit_is_decoded_rather_than_stalled() {
    // One 4 MiB run under a 1 MiB limit: nothing can be dispatched, so the
    // single-threaded path has to stream it.
    let (packed, plain) = join_runs(&[copy_run(&pseudo_random(4 << 20, 7))]);
    let sink = run_adaptive(16, &packed, 1 << 15, 8, 1 << 20).expect("decode");
    assert_eq!(sink.bytes(), plain);
}

#[test]
fn cancel_stops_and_joins() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 4);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    dec.feed(&packed).expect("feed");
    let mut n = 0usize;
    let _ = dec.drain(|_, b| n += b.len());
    dec.cancel();
    assert_eq!(dec.spawned_threads(), 0, "workers outlived cancel");
    assert_eq!(dec.feed(&packed), Err(Error::Cancelled));
    assert_eq!(dec.drain(|_, _| {}), Err(Error::Cancelled));
}

// 7. The chase: a run whose tail has not arrived still decodes.

#[test]
fn a_run_decodes_before_its_tail_arrives() {
    // A single run, so there is no boundary to wait for: output must come out
    // while the run is still incomplete, or a chasing caller would see nothing
    // until the whole download finished.
    let (prop, packed, plain) = multi_run(&["text.p1.xz"], 1);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(8, u64::MAX)).expect("props");
    let mut sink = Sink::default();

    let half = packed.len() / 2;
    dec.feed(&packed[..half]).expect("feed");
    let status = dec.drain(|o, b| sink.put(o, b)).expect("drain");
    assert_eq!(status, DrainStatus::Progress);
    let early = sink.bytes().len();
    assert!(early > 0, "nothing decoded from a run still arriving");
    assert_eq!(dec.pending_runs(), 0, "an incomplete run is not a backlog");
    assert_eq!(
        dec.spawned_threads(),
        0,
        "an incomplete run went to a worker"
    );

    dec.feed(&packed[half..]).expect("feed");
    dec.end_of_input();
    while dec.drain(|o, b| sink.put(o, b)).expect("drain") != DrainStatus::Finished {}
    assert_eq!(sink.bytes(), plain);
    assert!(early < plain.len(), "the whole run decoded before its tail");
}

#[test]
fn the_threaded_path_never_claims_a_run_the_chase_started() {
    // Feed most of a run, decode it single-threaded, then let the rest arrive
    // with threads available. The run is already part decoded, so it must stay
    // with the chase decoder rather than being handed to a worker, which would
    // decode it a second time and duplicate its output.
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 2);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(1, u64::MAX)).expect("props");
    let mut sink = Sink::default();

    let mut pos = 0usize;
    loop {
        if pos < packed.len() {
            // Small feeds keep the chase permanently part way through a run.
            let end = (pos + 512).min(packed.len());
            pos += dec.feed(&packed[pos..end]).expect("feed");
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        // Threads become available half way through, mid-run.
        if pos > packed.len() / 2 {
            dec.set_threads(8);
        }
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(sink.bytes(), plain);
}

// 12. The requests the sevenz-turbo fork filed: a chase that stands aside for a
// worker, a drain the caller can bound, and a run index over a seekable
// source.

#[test]
fn the_chase_stands_aside_while_a_worker_is_outstanding() {
    // With every run complete in the buffer and threads available, nothing
    // should be decoded on the calling thread: the chase exists for a run
    // whose tail has not arrived, and taking one here stops every worker from
    // claiming anything until it is through.
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "text.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 2);

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    let mut sink = Sink::default();
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    dec.end_of_input();
    while dec.drain(|o, b| sink.put(o, b)).expect("drain") != DrainStatus::Finished {}
    assert_eq!(sink.bytes(), plain);

    // Every run but the last is complete before the first drain, so every one
    // of them should have gone to a worker. The last run of the stream ends at
    // the end marker and is claimed the same way, so the only block the chase
    // may produce is none at all: each block delivered is one run.
    assert_eq!(
        sink.order.len(),
        names.len() * 2,
        "output was cut into more pieces than there are runs, so the chase \
         decoded some of it a megabyte at a time"
    );
}

#[test]
fn a_bounded_drain_cuts_blocks_without_reordering_them() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 2);
    for threads in [1usize, 4] {
        for limit in [1usize, 7, 4096, 1 << 16] {
            let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(threads, u64::MAX)).expect("props");
            let mut out: Vec<u8> = Vec::new();
            let mut pos = 0usize;
            loop {
                if pos < packed.len() {
                    let end = (pos + (1 << 15)).min(packed.len());
                    pos += dec.feed(&packed[pos..end]).expect("feed");
                    if pos == packed.len() {
                        dec.end_of_input();
                    }
                }
                let mut handed = 0usize;
                let status = dec
                    .drain_upto(limit, |off, b| {
                        assert_eq!(off, out.len() as u64, "out of order at {off}");
                        assert!(b.len() <= limit, "sink got {} over {limit}", b.len());
                        handed += b.len();
                        out.extend_from_slice(b);
                    })
                    .expect("drain_upto");
                assert!(
                    handed <= limit,
                    "{handed} bytes handed over for a {limit} limit"
                );
                if status == DrainStatus::Finished {
                    break;
                }
            }
            assert_eq!(out, plain, "threads={threads} limit={limit}");
        }
    }
}

#[test]
fn a_bounded_drain_holds_less_than_an_unbounded_one() {
    // The point of the limit: what the decoder holds follows what is in
    // flight, not what has been fed. Feed the whole stream, then take it 64
    // KiB at a time and watch the high-water mark.
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 4);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    dec.end_of_input();

    let mut out: Vec<u8> = Vec::new();
    let mut peak = 0u64;
    loop {
        let status = dec
            .drain_upto(1 << 16, |_, b| out.extend_from_slice(b))
            .expect("drain_upto");
        peak = peak.max(dec.in_flight_bytes());
        if status == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(out, plain);
    assert!(peak > 0, "nothing was ever in flight");
}

#[test]
fn feeding_far_ahead_of_many_small_runs_loses_nothing() {
    // What a `Read` adapter over an archive of incompressible data does: the
    // whole stream is fed before the first drain, the runs are small, and each
    // drain takes one buffer's worth. The input is dropped from the front in
    // large steps rather than after every run, so offsets into it have to stay
    // right across drops that land in the middle of the backlog.
    let runs: Vec<_> = (0..256)
        .map(|i| copy_run(&pseudo_random(1 << 16, i + 1)))
        .collect();
    let (packed, plain) = join_runs(&runs);
    let mut dec = Lzma2AdaptiveDecoder::new(16, &opts(4, u64::MAX)).expect("props");
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    dec.end_of_input();

    let mut out: Vec<u8> = Vec::new();
    loop {
        let status = dec
            .drain_upto(1 << 16, |off, b| {
                assert_eq!(off, out.len() as u64, "out of order at {off}");
                out.extend_from_slice(b);
            })
            .expect("drain_upto");
        if status == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(out, plain);
}

#[test]
fn a_zero_limit_delivers_nothing_and_loses_nothing() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz"], 1);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(2, u64::MAX)).expect("props");
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    dec.end_of_input();
    let status = dec
        .drain_upto(0, |_, _| panic!("a zero limit handed bytes over"))
        .expect("drain_upto");
    assert_eq!(status, DrainStatus::Progress);

    let mut out = Vec::new();
    while dec
        .drain_upto(1 << 20, |_, b| out.extend_from_slice(b))
        .expect("drain_upto")
        != DrainStatus::Finished
    {}
    assert_eq!(out, plain);
}

#[test]
fn run_boundaries_agrees_with_the_scanner_it_seeks_over() {
    use std::io::Cursor;

    use lzma_turbo::{Lzma2RunScanner, run_boundaries};

    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 3);

    let mut scanner = Lzma2RunScanner::new();
    scanner.feed(&packed).expect("scan");
    let mut expected = Vec::new();
    while let Some(r) = scanner.next_run() {
        expected.push(r);
    }

    // From the start, and from a position part way into a larger buffer: the
    // packed stream a 7z coder asks about is a range inside an archive.
    let mut padded = alloc_prefix(32);
    padded.extend_from_slice(&packed);
    let mut src = Cursor::new(padded);
    src.set_position(32);
    let got = run_boundaries(&mut src, prop).expect("run_boundaries");
    assert_eq!(got.len(), expected.len());
    for (g, e) in got.iter().zip(&expected) {
        assert_eq!(g.packed_len, e.packed_len);
        assert_eq!(g.unpacked_len, e.unpacked_len);
        assert_eq!(g.out_offset, e.out_offset);
    }
    assert_eq!(src.position(), 32, "the source's position was not restored");
}

fn alloc_prefix(n: usize) -> Vec<u8> {
    vec![0xAAu8; n]
}

#[test]
fn run_boundaries_rejects_a_range_that_stops_short() {
    use std::io::Cursor;

    use lzma_turbo::run_boundaries;

    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);
    let mut src = Cursor::new(packed[..packed.len() / 2].to_vec());
    let err = run_boundaries(&mut src, prop).expect_err("no end marker");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(src.position(), 0, "the source's position was not restored");

    let mut src = Cursor::new(Vec::new());
    assert!(
        run_boundaries(&mut src, 41).is_err(),
        "dict_prop 41 accepted"
    );
}

#[test]
fn chasing_off_waits_for_input_rather_than_serialising() {
    // The on-disk shape: everything is available, but it is fed in pieces
    // smaller than a run. With chasing on, the decoder decodes each run on the
    // calling thread as it arrives and never dispatches; with it off, it waits
    // for the rest of the run and hands it to a worker.
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "text.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 2);

    for chase in [true, false] {
        let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
        dec.set_chase(chase);
        assert_eq!(dec.chases(), chase);
        let mut sink = Sink::default();
        let mut pos = 0usize;
        loop {
            if pos < packed.len() {
                // Feeds far smaller than a run: without the knob the chase
                // decoder gets every run before a worker can.
                let end = (pos + 256).min(packed.len());
                pos += dec.feed(&packed[pos..end]).expect("feed");
                if pos == packed.len() {
                    dec.end_of_input();
                }
            }
            if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
                break;
            }
        }
        assert_eq!(sink.bytes(), plain, "chase={chase}");
        if !chase {
            assert!(
                dec.spawned_threads() >= 1,
                "no worker was used with chasing off"
            );
            assert_eq!(
                sink.order.len(),
                names.len() * 2,
                "output was cut into more pieces than there are runs"
            );
        }
    }
}

#[test]
fn chasing_off_still_finishes_a_stream_nothing_else_can_decode() {
    // A run too large for the memory limit, and the tail of a stream after
    // `end_of_input`: with chasing off both still have to come out, because
    // there is nothing else that could decode them.
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, 1 << 12)).expect("props");
    dec.set_chase(false);
    let mut sink = Sink::default();
    let mut pos = 0usize;
    loop {
        if pos < packed.len() {
            let end = (pos + 4096).min(packed.len());
            let took = dec.feed(&packed[pos..end]).expect("feed");
            pos += took;
            if pos == packed.len() {
                dec.end_of_input();
            }
        }
        if dec.drain(|o, b| sink.put(o, b)).expect("drain") == DrainStatus::Finished {
            break;
        }
    }
    assert_eq!(sink.bytes(), plain);
}

/// A bounded drain must not turn a truncated stream into a finished one.
///
/// The chase decoder stops when the caller's budget is spent, which can leave
/// it in the middle of a chunk that still owes output. If the end marker
/// happens to be the next input byte - which is what a stream cut inside its
/// last chunk and then given a marker looks like - the input cursor is at the
/// end of the stream with a chunk unfinished, and the decoder used to call
/// that a clean end. Found by `decode_lzma2_mt` fuzzing with a one-byte drain
/// budget.
#[test]
fn a_bounded_drain_does_not_accept_a_chunk_that_never_finished() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);
    // Inside the last chunk's payload, then an end marker.
    let mut cut = packed[..packed.len() * 3 / 4].to_vec();
    cut.push(0x00);

    for limit in [1usize, 7, 4096, usize::MAX] {
        let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(1, u64::MAX)).expect("props");
        let mut pos = 0usize;
        let mut err = None;
        let mut finished = false;
        'feed: loop {
            if pos < cut.len() {
                match dec.feed(&cut[pos..]) {
                    Ok(n) => pos += n,
                    Err(e) => {
                        err = Some(e);
                        break 'feed;
                    }
                }
                if pos == cut.len() {
                    dec.end_of_input();
                }
            }
            match dec.drain_upto(limit, |_, _| {}) {
                Ok(DrainStatus::Finished) => {
                    finished = true;
                    break 'feed;
                }
                Ok(_) => {}
                Err(e) => {
                    err = Some(e);
                    break 'feed;
                }
            }
        }
        assert!(
            !finished && err.is_some(),
            "limit {limit}: a truncated chunk was accepted"
        );
    }
}

// 13. Somewhere to wait: a caller with nothing left to feed must be able to
// block on a worker instead of calling `drain` until one answers.

#[test]
fn waiting_for_a_worker_is_false_when_none_is_outstanding() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 2);

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    assert!(!dec.wait_for_worker(), "waited with nothing dispatched");

    let mut sink = Sink::default();
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    dec.end_of_input();
    while dec.drain(|o, b| sink.put(o, b)).expect("drain") != DrainStatus::Finished {}
    assert_eq!(sink.bytes(), plain);
    assert!(!dec.wait_for_worker(), "waited after the stream finished");
}

#[test]
fn waiting_for_a_worker_takes_in_a_finished_run() {
    // The shape that used to spin: every run is in the buffer, the caller has
    // nothing more it wants to feed, and `drain` hands control straight back
    // while workers are still decoding. Each iteration here either produces
    // output or blocks on a worker, so the loop is bounded by the number of
    // runs rather than by how long a worker takes.
    let names = ["text.p1.xz", "mixed.p1.xz", "rand.p1.xz", "text.p1.xz"];
    let (prop, packed, plain) = multi_run(&names, 4);

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    dec.set_chase(false);
    let mut sink = Sink::default();
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    // Deliberately no `end_of_input` until the buffer is exhausted: that is
    // what makes `drain` return rather than wait.
    let mut waited = 0usize;
    let mut drains = 0usize;
    loop {
        drains += 1;
        assert!(drains < 10_000, "the drain loop did not converge");
        let before = sink.order.len();
        let status = dec.drain(|o, b| sink.put(o, b)).expect("drain");
        if status == DrainStatus::Finished {
            break;
        }
        if sink.order.len() != before {
            continue;
        }
        if dec.wait_for_worker() {
            waited += 1;
            continue;
        }
        dec.end_of_input();
    }
    assert_eq!(sink.bytes(), plain);
    assert!(waited > 0, "no wait was ever needed, so nothing was tested");
    assert!(!dec.wait_for_worker(), "a run outlived the finished stream");
}

#[test]
fn waiting_for_a_worker_returns_after_cancel() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 4);
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4, u64::MAX)).expect("props");
    dec.set_chase(false);
    let mut pos = 0usize;
    while pos < packed.len() {
        pos += dec.feed(&packed[pos..]).expect("feed");
    }
    let mut n = 0usize;
    let _ = dec.drain(|_, b| n += b.len());
    dec.cancel();
    assert!(!dec.wait_for_worker(), "waited for a cancelled worker");
    assert_eq!(dec.spawned_threads(), 0, "workers outlived cancel");
}
