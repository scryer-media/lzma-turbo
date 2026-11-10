//! The xz readers with block dictionaries on caller memory: the same streams
//! decoded on the decoder's own memory and on a [`WindowAllocator`]'s must
//! give the same bytes, and the allocator's memory must go back when the
//! decode is done with it. The raw LZMA and LZMA2 cases are unit tests in
//! `src/window.rs`, where Miri runs them.

#![cfg(all(feature = "enc", feature = "xz"))]

use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use lzma_turbo::xz::{
    CheckType, XzAdaptiveDecoder, XzError, XzErrorKind, XzOptions, XzParallelReader, XzReader,
};
use lzma_turbo::{
    LzmaEncProps, WindowAllocError, WindowAllocator, WindowBacking, WindowMemory, encode_xz,
};

struct HeapWindow {
    buf: Box<[u8]>,
    live: Arc<AtomicUsize>,
}

// SAFETY: a boxed slice is initialized, does not move when the box does, and
// is owned by this value alone until it drops.
unsafe impl WindowMemory for HeapWindow {
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.buf.as_mut_ptr()
    }
    fn len(&self) -> usize {
        self.buf.len()
    }
}

impl Drop for HeapWindow {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Heap windows full of garbage, with a count of those still alive and of
/// the most alive at once.
#[derive(Default)]
struct Heap {
    asked: Mutex<Vec<usize>>,
    calls: AtomicUsize,
    live: Arc<AtomicUsize>,
    peak: AtomicUsize,
    fail: bool,
    /// The first `n` calls meet here before any returns, each holding the
    /// window it made (or about to refuse): `n` asked for at once. No
    /// timing in it: a decode that asked for fewer at once would hang here,
    /// and the test runner ends a hung test.
    gate: Option<(usize, Barrier)>,
    /// The call, counting from zero, that is refused.
    refuse: Option<usize>,
}

impl WindowAllocator for Heap {
    fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(len);
        let gated = self.gate.as_ref().filter(|(n, _)| call < *n);
        if self.fail || self.refuse == Some(call) {
            if let Some((_, barrier)) = gated {
                barrier.wait();
            }
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "scratch disk full",
            ));
        }
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        let window = Box::new(HeapWindow {
            buf: (0..len)
                .map(|i| (i as u8).wrapping_mul(151) ^ 0x5A)
                .collect(),
            live: Arc::clone(&self.live),
        });
        if let Some((_, barrier)) = gated {
            barrier.wait();
        }
        Ok(window)
    }
}

impl Heap {
    fn asked(&self) -> Vec<usize> {
        self.asked.lock().unwrap().clone()
    }
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
    /// The first `n` calls are asked for at once.
    fn gated(n: usize) -> Self {
        Heap {
            gate: Some((n, Barrier::new(n))),
            ..Heap::default()
        }
    }
}

/// The kind of the located error an xz reader returned: its payload, or,
/// for a window the allocator refused, the allocator failure's location.
fn xz_kind(err: &io::Error) -> XzErrorKind {
    let inner = err.get_ref().expect("a payload");
    inner
        .downcast_ref::<XzError>()
        .copied()
        .or_else(|| inner.downcast_ref::<WindowAllocError>()?.xz_error())
        .expect("an XzError")
        .kind
}

/// `err` holds the allocator's own failure, located in the file.
fn allocator_error(err: io::Error, what: &str) -> WindowAllocError {
    assert_eq!(err.kind(), io::ErrorKind::StorageFull, "{what}: {err}");
    let w = WindowAllocError::from_io(err).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!(
        w.allocator_error().kind(),
        io::ErrorKind::StorageFull,
        "{what}"
    );
    assert_eq!(
        w.allocator_error().to_string(),
        "scratch disk full",
        "{what}"
    );
    let at = w.xz_error().expect("located");
    assert!(
        matches!(at.kind, XzErrorKind::WindowAlloc { .. }),
        "{what}: {at}"
    );
    w
}

fn backing(heap: &Arc<Heap>, in_memory_limit: u64) -> WindowBacking {
    WindowBacking::new(
        Arc::clone(heap) as Arc<dyn WindowAllocator>,
        in_memory_limit,
    )
}

fn sample(len: usize, seed: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed | 1;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        if x.is_multiple_of(4) && v.len() > 70_000 {
            let back = v.len() - 65_000 - (x as usize % 4000);
            let run: Vec<u8> = v[back..back + 64].to_vec();
            v.extend_from_slice(&run);
        } else {
            v.extend_from_slice(b"caller window ");
            v.push(x as u8);
        }
    }
    v.truncate(len);
    v
}

/// A 256 KiB dictionary, so a block of 300 KB wraps it.
fn xz(data: &[u8], block_size: u64) -> Vec<u8> {
    let props = LzmaEncProps::new().with_level(2).with_dict_size(256 << 10);
    encode_xz(data, &props, CheckType::Crc64, block_size).unwrap()
}

fn read_all(mut r: impl Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    r.read_to_end(&mut out)?;
    Ok(out)
}

fn owned(packed: &[u8]) -> Vec<u8> {
    read_all(XzReader::new(packed)).unwrap()
}

/// A window in the options leaves them and the readers with the auto traits
/// they have without one.
#[test]
fn xz_options_and_readers_keep_their_auto_traits() {
    use std::panic::{RefUnwindSafe, UnwindSafe};
    fn check<T: Send + Sync + Unpin + UnwindSafe + RefUnwindSafe>() {}
    check::<XzOptions>();
    check::<XzReader<&[u8]>>();
    // The parallel reader's thread pool was never `Sync` or unwind-safe.
    fn check_send<T: Send + Unpin>() {}
    check_send::<XzParallelReader<Cursor<Vec<u8>>>>();
}

#[test]
fn xz_single_and_multi_block_streams_match_owned_memory() {
    let data = sample(700_000, 7);
    for block_size in [u64::MAX, 300_000, 100_000] {
        let packed = xz(&data, block_size);
        assert_eq!(owned(&packed), data);
        let heap = Arc::new(Heap::default());
        let opts = XzOptions::default().with_window(backing(&heap, 0));
        let out = read_all(XzReader::with_options(packed.as_slice(), opts)).unwrap();
        assert_eq!(out, data, "block size {block_size}");
        let blocks = data
            .len()
            .div_ceil(block_size.min(data.len() as u64) as usize);
        assert_eq!(heap.asked().len(), blocks, "block size {block_size}");
        // One block's memory at a time, and none once the reader is done.
        assert_eq!(heap.peak.load(Ordering::SeqCst), 1);
        assert_eq!(heap.live(), 0);
    }
}

#[test]
fn concatenated_xz_streams_match_owned_memory() {
    let a = sample(400_000, 11);
    let b = sample(250_000, 13);
    let mut packed = xz(&a, 150_000);
    packed.extend_from_slice(&[0; 8]);
    packed.extend_from_slice(&xz(&b, u64::MAX));
    let whole: Vec<u8> = a.iter().chain(&b).copied().collect();
    assert_eq!(owned(&packed), whole);
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default().with_window(backing(&heap, 0));
    let out = read_all(XzReader::with_options(packed.as_slice(), opts)).unwrap();
    assert_eq!(out, whole);
    assert_eq!(heap.asked().len(), 3 + 1);
    assert_eq!(heap.live(), 0);
}

#[test]
fn only_blocks_over_the_in_memory_limit_go_to_the_allocator() {
    // The 256 KiB dictionary stays 262 144 bytes for a 300 000-byte block;
    // the 100 000-byte tail clamps it to 100 000, 102 400 once rounded.
    let data = sample(700_000, 17);
    let packed = xz(&data, 300_000);
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default().with_window(backing(&heap, 200_000));
    let out = read_all(XzReader::with_options(packed.as_slice(), opts)).unwrap();
    assert_eq!(out, data);
    assert_eq!(heap.asked(), [262_144, 262_144]);
}

#[test]
fn a_dictionary_over_the_memory_limit_needs_the_allocator() {
    let data = sample(400_000, 19);
    let packed = xz(&data, u64::MAX);
    let limit = 64 << 10;
    let refused = read_all(XzReader::with_options(
        packed.as_slice(),
        XzOptions::default().with_memory_limit(limit),
    ))
    .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::OutOfMemory);

    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default()
        .with_memory_limit(limit)
        .with_window(backing(&heap, limit));
    assert_eq!(
        read_all(XzReader::with_options(packed.as_slice(), opts)).unwrap(),
        data
    );
}

#[test]
fn an_allocator_failure_fails_the_xz_read_with_its_kind() {
    let data = sample(200_000, 23);
    let packed = xz(&data, u64::MAX);
    let heap = Arc::new(Heap {
        fail: true,
        ..Heap::default()
    });
    let opts = XzOptions::default().with_window(backing(&heap, 0));
    let mut reader = XzReader::with_options(packed.as_slice(), opts);
    let err = read_all(&mut reader).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert!(err.to_string().contains("window allocator"), "{err}");
    // A failed reader keeps failing.
    assert!(reader.read(&mut [0; 16]).is_err());
}

#[test]
fn a_corrupt_xz_block_releases_the_memory_when_the_reader_drops() {
    let data = sample(400_000, 29);
    let mut packed = xz(&data, u64::MAX);
    let mid = packed.len() / 2;
    for b in &mut packed[mid..mid + 32] {
        *b = !*b;
    }
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default().with_window(backing(&heap, 0));
    let mut reader = XzReader::with_options(packed.as_slice(), opts);
    assert!(read_all(&mut reader).is_err());
    assert_eq!(heap.asked().len(), 1);
    drop(reader);
    assert_eq!(heap.live(), 0);
}

/// The block sizes `xz(data, block)` writes, sorted.
fn block_sizes(data: &[u8], block: usize) -> Vec<usize> {
    let mut sizes: Vec<usize> = data.chunks(block).map(<[u8]>::len).collect();
    sizes.sort_unstable();
    sizes
}

fn sorted(mut v: Vec<usize>) -> Vec<usize> {
    v.sort_unstable();
    v
}

#[test]
fn the_parallel_reader_keeps_its_threads_and_gives_each_worker_a_window() {
    let data = sample(700_000, 31);
    let packed = xz(&data, 100_000);
    let serial = owned(&packed);
    assert_eq!(serial, data);
    for threads in [2, 3, 4] {
        let heap = Arc::new(Heap::gated(threads));
        let opts = XzOptions::default()
            .with_threads(threads)
            .with_window(backing(&heap, 0));
        let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
        assert_eq!(reader.threads(), threads);
        let out = read_all(reader).unwrap();
        assert!(out == serial, "{threads} threads");
        // One window per block, of the block's size. The first `threads`
        // were alive at once, and never more than that.
        assert_eq!(sorted(heap.asked()), block_sizes(&data, 100_000));
        assert_eq!(heap.peak(), threads, "{threads} threads");
        assert_eq!(heap.live(), 0);
    }
}

#[test]
fn verified_output_holds_its_blocks_on_lent_windows_too() {
    let data = sample(700_000, 37);
    let packed = xz(&data, 100_000);
    for threads in [1, 3] {
        let heap = Arc::new(Heap::default());
        let opts = XzOptions::default()
            .with_threads(threads)
            .with_verified_output(true)
            .with_window(backing(&heap, 0));
        let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
        assert_eq!(reader.threads(), threads);
        assert!(read_all(reader).unwrap() == data);
        assert_eq!(sorted(heap.asked()), block_sizes(&data, 100_000));
        assert!(heap.peak() <= threads);
        assert_eq!(heap.live(), 0);
    }
}

#[test]
fn one_thread_holds_one_window_at_a_time() {
    let data = sample(700_000, 41);
    let packed = xz(&data, 100_000);
    // In each block's dictionary, as `XzReader` decodes, and holding each
    // block whole until its check passes.
    for verified in [false, true] {
        let heap = Arc::new(Heap::default());
        let opts = XzOptions::default()
            .with_threads(1)
            .with_verified_output(verified)
            .with_window(backing(&heap, 0));
        let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
        assert_eq!(reader.threads(), 1);
        assert!(read_all(reader).unwrap() == data);
        assert_eq!(heap.asked().len(), 7, "verified {verified}");
        assert_eq!(heap.peak(), 1, "verified {verified}");
        assert_eq!(heap.live(), 0);
    }
    let heap = Arc::new(Heap::default());
    let mut dec = XzAdaptiveDecoder::new(
        XzOptions::default()
            .with_threads(1)
            .with_window(backing(&heap, 0)),
    );
    assert!(adaptive(&mut dec, &packed).unwrap() == data);
    assert_eq!(heap.asked().len(), 7);
    assert_eq!(heap.peak(), 1);
    drop(dec);
    assert_eq!(heap.live(), 0);
}

/// Feeds the whole file, then drains it, writing each piece at its offset.
fn adaptive(dec: &mut XzAdaptiveDecoder, packed: &[u8]) -> Result<Vec<u8>, XzError> {
    let mut fed = 0;
    while fed < packed.len() {
        let n = dec.feed(&packed[fed..])?;
        fed += n;
        if n == 0 {
            dec.drain(|_, _| unreachable!("nothing decodable before the input"))
                .ok();
        }
    }
    dec.end_of_input();
    let mut out = Vec::new();
    loop {
        let status = dec.drain(|offset, bytes| {
            assert_eq!(offset, out.len() as u64);
            out.extend_from_slice(bytes);
        })?;
        if status == lzma_turbo::DrainStatus::Finished {
            return Ok(out);
        }
    }
}

#[test]
fn the_adaptive_decoder_gives_each_worker_a_window() {
    let data = sample(700_000, 43);
    let packed = xz(&data, 100_000);
    for threads in [2, 3] {
        let heap = Arc::new(Heap::gated(threads));
        let mut dec = XzAdaptiveDecoder::new(
            XzOptions::default()
                .with_threads(threads)
                .with_window(backing(&heap, 0)),
        );
        assert!(adaptive(&mut dec, &packed).unwrap() == data);
        assert_eq!(dec.threads(), threads);
        assert_eq!(sorted(heap.asked()), block_sizes(&data, 100_000));
        assert_eq!(heap.peak(), threads, "{threads} threads");
        drop(dec);
        assert_eq!(heap.live(), 0);
    }
}

#[test]
fn a_refused_second_window_fails_the_threaded_decode() {
    let data = sample(700_000, 47);
    let packed = xz(&data, 100_000);
    // The two first windows are asked for at once, and the second refused.
    let heap = Arc::new(Heap {
        refuse: Some(1),
        ..Heap::gated(2)
    });
    let opts = XzOptions::default()
        .with_threads(2)
        .with_window(backing(&heap, 0));
    let mut reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
    assert_eq!(reader.threads(), 2);
    let mut out = Vec::new();
    let err = reader.read_to_end(&mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert!(
        matches!(
            xz_kind(&err),
            XzErrorKind::WindowAlloc {
                needed: 100_000,
                kind: io::ErrorKind::StorageFull,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(allocator_error(err, "parallel").needed(), 100_000);
    // Whatever was delivered before the failure is the file's start, and
    // the reader stays failed.
    assert!(out.len() <= 100_000 && data.starts_with(&out));
    assert!(reader.read(&mut [0; 16]).is_err());
    drop(reader);
    assert_eq!(heap.live(), 0);

    let heap = Arc::new(Heap {
        refuse: Some(1),
        ..Heap::gated(2)
    });
    let mut dec = XzAdaptiveDecoder::new(
        XzOptions::default()
            .with_threads(2)
            .with_window(backing(&heap, 0)),
    );
    let err = adaptive(&mut dec, &packed).unwrap_err();
    assert!(
        matches!(
            err.kind,
            XzErrorKind::WindowAlloc {
                needed: 100_000,
                kind: io::ErrorKind::StorageFull,
                ..
            }
        ),
        "{err}"
    );
    let w = dec.take_window_error().expect("the allocator's error");
    assert_eq!(w.xz_error(), Some(err));
    assert_eq!(w.allocator_error().to_string(), "scratch disk full");
    assert!(dec.take_window_error().is_none());
    drop(dec);
    assert_eq!(heap.live(), 0);
}

/// Every xz decoder that takes a window hands back the allocator's own
/// error when it refuses one, located in the file: the readers inside the
/// `io::Error` they return, the adaptive decoder from `take_window_error`.
/// On one thread and on two, and every window goes back.
#[test]
fn every_xz_decoder_hands_back_the_allocators_error() {
    let data = sample(300_000, 59);
    let packed = xz(&data, 100_000);
    let failing = || {
        Arc::new(Heap {
            fail: true,
            ..Heap::default()
        })
    };

    let heap = failing();
    let opts = XzOptions::default().with_window(backing(&heap, 0));
    let mut reader = XzReader::with_options(packed.as_slice(), opts);
    allocator_error(read_all(&mut reader).unwrap_err(), "XzReader");
    drop(reader);
    assert_eq!(heap.live(), 0);

    for threads in [1, 2] {
        let what = format!("XzParallelReader, {threads} threads");
        let heap = failing();
        let opts = XzOptions::default()
            .with_threads(threads)
            .with_window(backing(&heap, 0));
        let mut reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
        allocator_error(read_all(&mut reader).unwrap_err(), &what);
        // The reader stays failed, with the located error.
        let again = reader.read(&mut [0; 16]).unwrap_err();
        assert!(
            matches!(xz_kind(&again), XzErrorKind::WindowAlloc { .. }),
            "{what}"
        );
        drop(reader);
        assert_eq!(heap.live(), 0, "{what}");

        let what = format!("XzAdaptiveDecoder, {threads} threads");
        let heap = failing();
        let mut dec = XzAdaptiveDecoder::new(
            XzOptions::default()
                .with_threads(threads)
                .with_window(backing(&heap, 0)),
        );
        let err = adaptive(&mut dec, &packed).unwrap_err();
        assert!(
            matches!(err.kind, XzErrorKind::WindowAlloc { .. }),
            "{what}: {err}"
        );
        let w = dec.take_window_error().expect("the allocator's error");
        assert_eq!(w.xz_error(), Some(err), "{what}");
        assert_eq!(
            w.allocator_error().to_string(),
            "scratch disk full",
            "{what}"
        );
        drop(dec);
        assert_eq!(heap.live(), 0, "{what}");
    }
}

#[test]
fn the_memory_limit_counts_only_what_stays_in_ram() {
    let data = sample(700_000, 53);
    let packed = xz(&data, 100_000);
    let estimate = |window: Option<WindowBacking>, limit: u64| {
        let mut opts = XzOptions::default()
            .with_threads(4)
            .with_memory_limit(limit);
        if let Some(w) = window {
            opts = opts.with_window(w);
        }
        XzParallelReader::with_options(Cursor::new(&packed), opts)
            .map(|r| (r.threads(), r.memory_estimate()))
    };
    let heap = Arc::new(Heap::default());
    let (_, in_ram) = estimate(None, u64::MAX).unwrap();
    let (threads, lent) = estimate(Some(backing(&heap, 0)), u64::MAX).unwrap();
    assert_eq!(threads, 4);
    // Four workers' packed blocks and probabilities, and no output buffer.
    assert!(lent * 3 < in_ram, "{lent} against {in_ram}");
    // A limit that pays for four lent workers keeps four threads; without
    // the window it would pay for fewer.
    assert_eq!(estimate(Some(backing(&heap, 0)), lent).unwrap(), (4, lent));
    assert!(estimate(None, lent).unwrap().0 < 4);
    assert!(heap.asked().is_empty());
}

#[test]
fn a_window_over_the_ceiling_is_refused_without_asking() {
    let data = sample(700_000, 59);
    let packed = xz(&data, 200_000);
    let heap = Arc::new(Heap::default());
    let capped = || backing(&heap, 0).with_max_len(150_000);

    // The threaded reader holds 200 000-byte blocks, and refuses them before
    // it starts.
    let err = XzParallelReader::with_options(
        Cursor::new(&packed),
        XzOptions::default().with_threads(2).with_window(capped()),
    )
    .err()
    .expect("a block over the ceiling");
    assert_eq!(err.kind, XzErrorKind::memory_limit(200_000, 150_000));

    // The adaptive workers refuse the block when it arrives.
    let mut dec =
        XzAdaptiveDecoder::new(XzOptions::default().with_threads(2).with_window(capped()));
    let err = adaptive(&mut dec, &packed).unwrap_err();
    assert_eq!(err.kind, XzErrorKind::memory_limit(200_000, 150_000));

    // The sequential reader's dictionary, clamped to the 200 000-byte
    // block and rounded to 200 704, is over it too.
    let err = read_all(XzReader::with_options(
        packed.as_slice(),
        XzOptions::default().with_window(capped()),
    ))
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    assert_eq!(xz_kind(&err), XzErrorKind::memory_limit(200_704, 150_000));
    assert!(heap.asked().is_empty());

    // Under the in-RAM limit the window changes nothing.
    let opts = XzOptions::default()
        .with_threads(4)
        .with_window(backing(&heap, u64::MAX));
    let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
    assert_eq!(reader.threads(), 4);
    assert!(read_all(reader).unwrap() == data);
    assert!(heap.asked().is_empty());
}
