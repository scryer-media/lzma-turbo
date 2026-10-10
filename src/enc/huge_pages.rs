//! Huge-page advice for the encoder's large buffers.
//!
//! C: what `BigAlloc` in `C/Alloc.c` does on Linux for an allocation of more
//! than 1 MiB - `madvise(MADV_HUGEPAGE)` - which the SDK's encoder asks for
//! its match finder's window, hash and son tables, the threaded finder's
//! buffers and each block thread's input. The allocation itself is the
//! ordinary one here: only the 2 MiB-aligned part of it inside the buffer is
//! advised, and the advice is best-effort. A kernel that refuses it, or one
//! without transparent huge pages, leaves the buffer as it was.

/// The huge-page size the advice is aligned to. C: `LARGE_PAGE_SIZE_DEFAULT`.
const HUGE_PAGE: usize = 1 << 21;

/// Buffers no larger than this are not advised. C: `g_LargePageThresholdMin`.
const THRESHOLD: usize = HUGE_PAGE / 2;

/// The whole huge pages inside `[addr, addr + len)`, as a start and a length,
/// or `None` when the buffer is no larger than the threshold or holds no whole
/// huge page.
fn interior(addr: usize, len: usize) -> Option<(usize, usize)> {
    if len <= THRESHOLD {
        return None;
    }
    let end = addr.checked_add(len)?;
    let start = addr.checked_add(HUGE_PAGE - 1)? & !(HUGE_PAGE - 1);
    let stop = end & !(HUGE_PAGE - 1);
    (stop > start).then(|| (start, stop - start))
}

/// Advise the huge pages inside `[addr, addr + len)` through `call`, which
/// is given a start and a length and returns what `madvise` returns. Whatever
/// it returns is ignored: the advice never fails the allocation it is for.
/// Returns whether `call` was made.
fn advise_with(addr: usize, len: usize, call: impl FnOnce(usize, usize) -> i32) -> bool {
    match interior(addr, len) {
        Some((start, length)) => {
            let _ = call(start, length);
            true
        }
        None => false,
    }
}

/// Advise huge pages for the whole of `v`'s allocation, its spare capacity
/// included, so a buffer advised before it is filled is faulted in huge pages.
pub(crate) fn advise_vec<T>(v: &alloc::vec::Vec<T>) {
    let len = v.capacity().saturating_mul(core::mem::size_of::<T>());
    advise_with(v.as_ptr() as usize, len, madvise_hugepage);
}

#[cfg(all(target_os = "linux", feature = "std", not(miri)))]
fn madvise_hugepage(start: usize, length: usize) -> i32 {
    // asm-generic/mman-common.h.
    const MADV_HUGEPAGE: core::ffi::c_int = 14;
    unsafe extern "C" {
        fn madvise(
            addr: *mut core::ffi::c_void,
            length: usize,
            advice: core::ffi::c_int,
        ) -> core::ffi::c_int;
    }
    // SAFETY: `MADV_HUGEPAGE` changes neither the contents nor the mapping of
    // the range: it only lets the kernel back it with huge pages. The range
    // is page-aligned (`interior` aligns it to 2 MiB) and lies inside the
    // caller's allocation; a range the kernel will not advise comes back as
    // an error, which `advise_with` ignores.
    unsafe { madvise(start as *mut core::ffi::c_void, length, MADV_HUGEPAGE) }
}

/// Elsewhere there is nothing to advise.
#[cfg(not(all(target_os = "linux", feature = "std", not(miri))))]
fn madvise_hugepage(_start: usize, _length: usize) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small and unaligned buffers are advised only on the huge pages they
    /// hold whole, and a buffer holding none is not advised at all.
    #[test]
    fn the_advice_covers_only_whole_huge_pages_inside_the_buffer() {
        let mb = 1usize << 20;
        // At or below the threshold: never advised, however aligned.
        assert_eq!(interior(0, 0), None);
        assert_eq!(interior(HUGE_PAGE, THRESHOLD), None);
        // Above it but holding no whole huge page.
        assert_eq!(interior(HUGE_PAGE + 16, HUGE_PAGE), None);
        assert_eq!(interior(HUGE_PAGE, HUGE_PAGE - 1), None);
        // Aligned: all of it.
        assert_eq!(interior(HUGE_PAGE, 4 * mb), Some((HUGE_PAGE, 4 * mb)));
        // Unaligned at both ends: the interior.
        assert_eq!(
            interior(HUGE_PAGE + 16, 6 * mb),
            Some((2 * HUGE_PAGE, 4 * mb))
        );
        // A range that would wrap the address space is not advised.
        assert_eq!(interior(usize::MAX - mb, 4 * mb), None);
    }

    /// Whatever the system call returns - success, `EINVAL` from a kernel
    /// without transparent huge pages, `ENOSYS` - the advice is made once and
    /// its result dropped.
    #[test]
    fn a_refused_advice_is_ignored() {
        for ret in [0, -1] {
            let mut seen = None;
            let made = advise_with(HUGE_PAGE + 16, 6 << 20, |s, l| {
                seen = Some((s, l));
                ret
            });
            assert!(made);
            assert_eq!(seen, Some((2 * HUGE_PAGE, 4 << 20)));
        }
        let made = advise_with(16, 4096, |_, _| panic!("a small buffer is never advised"));
        assert!(!made);
    }

    /// The real advice on real buffers - empty, small, and large and still
    /// unfilled - leaves each usable and its contents as they were.
    #[test]
    fn advising_a_buffer_leaves_it_usable() {
        let empty: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        advise_vec(&empty);
        let small = alloc::vec![7u8; 100];
        advise_vec(&small);
        assert!(small.iter().all(|&b| b == 7));
        let mut big: alloc::vec::Vec<u32> = alloc::vec::Vec::with_capacity(3 << 20);
        advise_vec(&big);
        big.resize(3 << 20, 9);
        assert!(big.iter().all(|&w| w == 9));
    }
}
