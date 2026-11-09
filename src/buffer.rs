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
    // allocation and deallocates it with the same layout. The test below
    // covers this under Miri.
    Ok(unsafe { Vec::from_raw_parts(ptr, len, len) })
}

/// The decoder's dictionary: its own zeroed allocation, or memory a caller
/// lent it through [`crate::window::WindowAllocator`].
///
/// Either way it is used through the same two shapes the decoder always
/// used: a raw base pointer for the fast loops, and a byte slice for the
/// rest.
pub(crate) enum DicBuf {
    Owned(Vec<u8>),
    #[cfg(feature = "std")]
    Caller(crate::window::CallerWindow),
}

impl Default for DicBuf {
    fn default() -> Self {
        DicBuf::Owned(Vec::new())
    }
}

impl DicBuf {
    /// The base pointer the decode loops index from. `Vec::as_mut_ptr`
    /// rather than a slice's, so no reference is made on the way.
    #[inline(always)]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            DicBuf::Owned(v) => v.as_mut_ptr(),
            #[cfg(feature = "std")]
            DicBuf::Caller(c) => c.as_mut_ptr(),
        }
    }

    /// Gives back an owned buffer, leaving an empty one. Caller memory is
    /// released and an empty `Vec` returned in its place: only the threaded
    /// decoders hand buffers back, and they never run on caller memory.
    #[cfg(feature = "std")]
    pub(crate) fn take_vec(&mut self) -> Vec<u8> {
        match core::mem::take(self) {
            DicBuf::Owned(v) => v,
            DicBuf::Caller(_) => Vec::new(),
        }
    }
}

impl core::ops::Deref for DicBuf {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        match self {
            DicBuf::Owned(v) => v,
            #[cfg(feature = "std")]
            DicBuf::Caller(c) => c.as_slice(),
        }
    }
}

impl core::ops::DerefMut for DicBuf {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            DicBuf::Owned(v) => v,
            #[cfg(feature = "std")]
            DicBuf::Caller(c) => c.as_mut_slice(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// A zeroed allocation of every [`super::Zeroable`] type is the length
    /// asked for, all zero and writable to its last element, at odd lengths
    /// and across a page; an empty one is empty, and one the allocator
    /// cannot describe is refused. CI runs this under Miri too, which checks
    /// the allocation, the initialisation and the free against the layout.
    #[test]
    fn a_zeroed_vec_is_zeroes_of_every_zeroable_type() {
        fn check<T: super::Zeroable + PartialEq + core::fmt::Debug>(one: T, zero: T) {
            for len in [1usize, 3, 7, 61, 4_097, 70_001] {
                let mut v = super::zeroed_vec::<T>(len).unwrap();
                assert_eq!((v.len(), v.capacity()), (len, len));
                assert!(v.iter().all(|&x| x == zero), "{len}");
                v[len - 1] = one;
                v[0] = one;
                assert_eq!((v[0], v[len - 1]), (one, one));
                v.push(one);
                assert_eq!(v.len(), len + 1);
            }
            assert!(super::zeroed_vec::<T>(0).unwrap().is_empty());
            assert!(matches!(
                super::zeroed_vec::<T>(usize::MAX),
                Err(crate::Error::Alloc)
            ));
        }
        check::<u8>(0xA5, 0);
        check::<u32>(0xDEAD_BEEF, 0);
    }
}
