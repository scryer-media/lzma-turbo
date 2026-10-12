//! Allocation failures must be recoverable without changing encoder settings.
#![cfg(all(feature = "std", feature = "enc"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use lzma_turbo::{Error, Lzma2Encoder, LzmaEncProps, LzmaEncoder, MatchFinderKind, SliceStream};

thread_local! {
    static REFUSE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
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
}

#[global_allocator]
static ALLOCATOR: Fallible = Fallible;

struct Refusal;

impl Refusal {
    fn new(size: usize, align: usize) -> Self {
        FAILURES.set(0);
        REFUSE.set(Some((size, align)));
        Self
    }
}

impl Drop for Refusal {
    fn drop(&mut self) {
        REFUSE.set(None);
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
