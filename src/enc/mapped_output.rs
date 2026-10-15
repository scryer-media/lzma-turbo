//! Exclusively owned anonymous storage for large, borrowed encoder output.
//! This Darwin allocation policy is an extension to the SDK, not a change
//! to its encoder. Retiring a mapping releases pages instead of leaving them
//! in the system allocator's freed-block cache. It never backs a public Vec.

use crate::error::Error;
use core::{
    ffi::{c_int, c_void},
    ptr::NonNull,
};

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

pub(super) struct MappedOutput {
    address: NonNull<u8>,
    len: usize,
}

// SAFETY: this object exclusively owns its mapping. Mutable slices borrow
// it exclusively, shared slices borrow it immutably, and Drop runs once.
unsafe impl Send for MappedOutput {}

impl MappedOutput {
    pub(super) fn new(len: usize) -> Result<Self, Error> {
        if len == 0 || len > isize::MAX as usize {
            return Err(Error::Alloc);
        }
        // SAFETY: Darwin MAP_PRIVATE | MAP_ANON with no fixed address creates
        // a fresh, zero-filled mapping. PROT_READ | PROT_WRITE allows byte
        // access; fd -1 and offset 0 select no file or special VM flags.
        // mmap rounds the mapping to pages; only the requested length is
        // exposed. The same address/length pair is passed to munmap on Drop.
        let raw = unsafe { mmap(core::ptr::null_mut(), len, 3, 0x1002, -1, 0) };
        if raw.addr() == usize::MAX {
            return Err(Error::Alloc);
        }
        let Some(address) = NonNull::new(raw.cast()) else {
            // SAFETY: a successful mapping at zero cannot form a Rust slice,
            // but is still owned here and must be released with its length.
            unsafe {
                munmap(raw, len);
            }
            return Err(Error::Alloc);
        };
        Ok(Self { address, len })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: new establishes a non-null, initialized mapping of len
        // bytes, bounded by isize::MAX. This borrow cannot outlive its owner.
        unsafe { core::slice::from_raw_parts(self.address.as_ptr(), self.len) }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as bytes(), with an exclusive owner borrow preventing any
        // competing slice access or deallocation for this slice's lifetime.
        unsafe { core::slice::from_raw_parts_mut(self.address.as_ptr(), self.len) }
    }
}

impl Drop for MappedOutput {
    fn drop(&mut self) {
        // SAFETY: the mapping is exclusively owned, has not been unmapped,
        // and no slice can survive this owner's destruction.
        unsafe {
            munmap(self.address.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_output_is_zeroed_bounded_and_owned_across_threads() {
        assert!(MappedOutput::new(0).is_err());
        assert!(MappedOutput::new(usize::MAX).is_err());
        for len in [1, 16383, 16384, 16385, (9 << 20) + 19] {
            let mut first = MappedOutput::new(len).unwrap();
            let mut neighbor = MappedOutput::new(len).unwrap();
            assert!(first.bytes().iter().all(|&b| b == 0));
            first.bytes_mut().fill(0x5a);
            neighbor.bytes_mut().fill(0xa5);
            std::thread::spawn(move || {
                assert!(first.bytes().iter().all(|&b| b == 0x5a));
            })
            .join()
            .unwrap();
            assert!(neighbor.bytes().iter().all(|&b| b == 0xa5));
        }
    }
}
