//! Allocation failures must be recoverable without changing encoder settings.
#![cfg(all(feature = "std", feature = "enc"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use lzma_turbo::{
    Error, Lzma2Decoder, Lzma2Encoder, LzmaDecoder, LzmaEncProps, LzmaEncoder, LzmaProps,
    MatchFinderKind, SliceStream,
};

#[test]
fn decoder_dictionary_allocation_failure_is_recoverable() {
    let props = LzmaProps::new(3, 0, 2, 1 << 16).unwrap();
    let refusal = Refusal::new(1 << 16, 1);
    let lzma = LzmaDecoder::new(props);
    let lzma2 = Lzma2Decoder::new(8);
    drop(refusal);
    assert!(matches!(lzma, Err(Error::Alloc)));
    assert!(matches!(lzma2, Err(Error::Alloc)));
    assert_eq!(FAILURES.get(), 2);
    assert!(LzmaDecoder::new(props).is_ok());
    assert!(Lzma2Decoder::new(8).is_ok());
}

thread_local! {
    static REFUSE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    static REFUSE_SHRINK: Cell<Option<usize>> = const { Cell::new(None) };
    static FAILURES: Cell<usize> = const { Cell::new(0) };
}

struct Fallible;

fn refused(layout: Layout) -> bool {
    let refuse = REFUSE
        .try_with(|slot| slot.get() == Some((layout.size(), layout.align())))
        .unwrap_or(false);
    if refuse {
        let _ = FAILURES.try_with(|count| count.set(count.get() + 1));
    }
    refuse
}

// SAFETY: successful allocations and every deallocation use System with
// unchanged layouts. Refusal returns null, the GlobalAlloc failure contract.
unsafe impl GlobalAlloc for Fallible {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if refused(layout) {
            return core::ptr::null_mut();
        }
        // SAFETY: GlobalAlloc's caller supplies a valid nonzero layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if refused(layout) {
            return core::ptr::null_mut();
        }
        // SAFETY: as alloc; System also supplies the requested zero fill.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: every non-null allocation came from System with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size < layout.size()
            && REFUSE_SHRINK
                .try_with(|slot| slot.get() == Some(layout.size()))
                .unwrap_or(false)
        {
            let _ = FAILURES.try_with(|count| count.set(count.get() + 1));
            return core::ptr::null_mut();
        }
        // SAFETY: the original System allocation and layout are unchanged;
        // GlobalAlloc's caller supplies a valid, nonzero new size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Fallible = Fallible;

struct Refusal;

impl Refusal {
    fn shrink(capacity: usize) -> Self {
        FAILURES.set(0);
        REFUSE_SHRINK.set(Some(capacity));
        Self
    }

    fn new(size: usize, align: usize) -> Self {
        FAILURES.set(0);
        REFUSE.set(Some((size, align)));
        Self
    }
}

impl Drop for Refusal {
    fn drop(&mut self) {
        REFUSE.set(None);
        REFUSE_SHRINK.set(None);
    }
}

#[test]
fn result_shrink_failure_preserves_solid_and_parallel_output() {
    const BLOCK: usize = 1 << 16;
    let props = LzmaEncProps::new().with_dict_size(1 << 15);
    // Empty block-thread input takes the single-thread Vec writer rather
    // than the bounded parallel destination, so exercise that shrink with
    // enough data to dispatch multiple blocks.
    for (src, parallel) in [
        (Vec::new(), false),
        (vec![b'z'; 3 * BLOCK + 123], false),
        (vec![b'z'; 3 * BLOCK + 123], true),
    ] {
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        if parallel {
            enc.set_block_size(BLOCK as u64);
            enc.set_threads(3);
        }
        let want = enc.encode_to_vec(&src).unwrap();
        assert_eq!(want.capacity(), want.len(), "successful shrink");
        let capacity = if parallel {
            let blocks = src.len().div_ceil(BLOCK).max(1);
            let tail = src.len() - (blocks - 1) * BLOCK;
            (blocks - 1) * (BLOCK + (BLOCK >> 10) + 16) + tail + (tail >> 10) + 16
        } else {
            src.len() + (src.len() >> 10) + 16
        };
        let refusal = Refusal::shrink(capacity);
        let result = enc.encode_to_vec(&src);
        drop(refusal);
        assert_eq!(FAILURES.get(), 1, "must exercise the final shrink");
        let got = result.expect("shrinking is optional after successful encoding");
        assert_eq!(got.capacity(), capacity, "keep the original allocation");
        assert_eq!(got, want, "parallel={parallel}, input={}", src.len());
    }
}

#[test]
fn failed_reconfiguration_keeps_the_allocated_coder_usable() {
    let props = LzmaEncProps::new().with_dict_size(1 << 15);
    let src = vec![b'x'; 100_000];
    let mut enc = Lzma2Encoder::new(&props).unwrap();
    // Establish the live coder under the same finite cap used for replacement.
    enc.set_mem_limit(64 << 20);
    let mut want = Vec::new();
    enc.encode(&mut SliceStream::new(&src), &mut want).unwrap();
    let property = enc.properties();
    let mut got = Vec::with_capacity(want.len() + 64);

    // RangeEnc's first reservation in a replacement coder, kept unavailable
    // through the next encode. The live coder already owns that buffer.
    let refusal = Refusal::new(1 << 16, 1);
    let result = enc.set_props(&props.with_dict_size(1 << 16));
    let encode = enc.encode(&mut SliceStream::new(&src), &mut got);
    drop(refusal);
    assert_eq!(result, Err(Error::Alloc));
    assert_eq!(FAILURES.get(), 1, "must not rebuild the old coder");
    assert_eq!(encode, Ok(()));
    assert_eq!(enc.properties(), property);
    assert_eq!(got, want);
}

#[test]
fn hash_chain_head_allocation_failure_is_recoverable() {
    let src = vec![b'y'; 100_000];
    for kind in [MatchFinderKind::Hc4, MatchFinderKind::Hc5] {
        let props = LzmaEncProps::new()
            .with_dict_size(1 << 15)
            .with_match_finder(kind);
        let mut enc = LzmaEncoder::new(&props).unwrap();
        let mut got = Vec::new();
        // The HC head batch is exactly 4096 u32s. Other encoder allocations
        // have different layouts, and the constructor has already succeeded.
        let refusal = Refusal::new(4096 * 4, 4);
        let result = enc.encode(&mut SliceStream::new(&src), &mut got);
        drop(refusal);
        assert_eq!(result, Err(Error::Alloc));
        assert_eq!(FAILURES.get(), 1);
        got.clear();
        enc.encode(&mut SliceStream::new(&src), &mut got).unwrap();
        let mut want = Vec::new();
        LzmaEncoder::new(&props)
            .unwrap()
            .encode(&mut SliceStream::new(&src), &mut want)
            .unwrap();
        assert_eq!(got, want, "{kind:?}");
    }
}
