use alloc::vec::Vec;

use crate::Error;

/// A type whose every value may be the all-zero bit pattern.
///
/// # Safety
///
/// Implement only for types for which all-zero bytes are a valid value.
pub(crate) unsafe trait Zeroable: Copy {}
// SAFETY: every bit pattern is valid for an unsigned integer.
unsafe impl Zeroable for u8 {}
// SAFETY: every bit pattern is valid for an unsigned integer.
unsafe impl Zeroable for u32 {}

// Allocate initialized zeroes without eagerly writing pages that the allocator
// already knows are zero. C: LzmaDec_Allocate and MatchFinder_Create allocate
// dictionaries before input determines how much of them is actually used.
pub(crate) fn zeroed_vec<T: Zeroable>(len: usize) -> Result<Vec<T>, Error> {
    if len == 0 || core::mem::size_of::<T>() == 0 {
        return Ok(Vec::new());
    }
    let layout = core::alloc::Layout::array::<T>(len).map_err(|_| Error::Alloc)?;
    // SAFETY: the checks above establish a non-zero allocation size.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) } as *mut T;
    if ptr.is_null() {
        return Err(Error::Alloc);
    }
    // SAFETY: the global allocator returned room and alignment for exactly len
    // initialized values: Zeroable guarantees that zero is valid. Vec owns the
    // allocation and deallocates it with the same layout. The encoder's
    // a_zeroed_vec_is_zeroes_of_every_zeroable_type test covers this under Miri.
    Ok(unsafe { Vec::from_raw_parts(ptr, len, len) })
}
