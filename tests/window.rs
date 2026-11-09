//! The xz readers with block dictionaries on caller memory: the same streams
//! decoded on the decoder's own memory and on a [`WindowAllocator`]'s must
//! give the same bytes, and the allocator's memory must go back when the
//! decode is done with it. The raw LZMA and LZMA2 cases are unit tests in
//! `src/window.rs`, where Miri runs them.

#![cfg(all(feature = "enc", feature = "xz"))]

use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lzma_turbo::xz::{CheckType, XzOptions, XzParallelReader, XzReader};
use lzma_turbo::{LzmaEncProps, WindowAllocator, WindowBacking, WindowMemory, encode_xz};

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
    live: Arc<AtomicUsize>,
    peak: AtomicUsize,
    fail: bool,
}

impl WindowAllocator for Heap {
    fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>> {
        self.asked.lock().unwrap().push(len);
        if self.fail {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "scratch disk full",
            ));
        }
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        Ok(Box::new(HeapWindow {
            buf: (0..len)
                .map(|i| (i as u8).wrapping_mul(151) ^ 0x5A)
                .collect(),
            live: Arc::clone(&self.live),
        }))
    }
}

impl Heap {
    fn asked(&self) -> Vec<usize> {
        self.asked.lock().unwrap().clone()
    }
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
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
        if x % 4 == 0 && v.len() > 70_000 {
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

#[test]
fn the_parallel_reader_decodes_a_windowed_file_on_one_thread() {
    let data = sample(700_000, 31);
    let packed = xz(&data, 200_000);
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default()
        .with_threads(4)
        .with_window(backing(&heap, 0));
    let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
    assert_eq!(reader.threads(), 1);
    assert_eq!(read_all(reader).unwrap(), data);
    assert_eq!(heap.asked().len(), 4);
    assert_eq!(heap.live(), 0);

    // Under the in-RAM limit the window changes nothing.
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default()
        .with_threads(4)
        .with_window(backing(&heap, u64::MAX));
    let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
    assert_eq!(reader.threads(), 4);
    assert_eq!(read_all(reader).unwrap(), data);
    assert!(heap.asked().is_empty());

    // A verified decode holds whole blocks in RAM and ignores the window.
    let heap = Arc::new(Heap::default());
    let opts = XzOptions::default()
        .with_threads(4)
        .with_verified_output(true)
        .with_window(backing(&heap, 0));
    let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).unwrap();
    assert_eq!(reader.threads(), 4);
    assert_eq!(read_all(reader).unwrap(), data);
    assert!(heap.asked().is_empty());
}
