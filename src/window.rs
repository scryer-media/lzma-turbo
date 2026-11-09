//! Caller-supplied memory for the decoder's dictionary.
//!
//! A decoder keeps the last `dictionary size` bytes it produced, because a
//! match can copy from anywhere in them. That buffer - the window - is the
//! decoder's one large allocation: an `xz -9` stream asks for 64 MiB, and a
//! 7z or LZMA2 stream can ask for up to 4 GiB. An application that would
//! rather not hold that in RAM can back it with memory of its own, a mapped
//! scratch file for example, through a [`WindowAllocator`].
//!
//! [`WindowBacking`] pairs the allocator with an in-RAM limit and a ceiling.
//! A dictionary at or under the limit is allocated by the decoder exactly as
//! without one; a dictionary over it is asked of the allocator, unless it is
//! also over the ceiling, which refuses it with the memory-limit error. The
//! readers that take one are [`crate::LzmaReader`], [`crate::Lzma2Reader`]
//! and, through `XzOptions::with_window`, the xz readers. A threaded xz
//! decode keeps the thread count the caller set and asks the allocator once
//! per block in flight.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::fmt;
use core::panic::AssertUnwindSafe;
use std::io;

/// Memory a [`WindowAllocator`] lends a decoder for its dictionary.
///
/// The decoder calls [`WindowMemory::as_mut_ptr`] and [`WindowMemory::len`]
/// once each, when it receives the value, and reads and writes through that
/// pointer until it drops the value.
///
/// # Safety
///
/// An implementation must guarantee all of the following, from the call to
/// [`WindowMemory::as_mut_ptr`] until the value is dropped:
///
/// - the pointer is non-null and valid for reads and writes of `len()`
///   bytes, which is at least the `len` [`WindowAllocator::allocate`] was
///   asked for; the decoder uses exactly the first `len` asked for;
/// - those bytes are initialized, to any value: they need not be zero;
/// - the address is stable until drop, including when the `Box` holding
///   the value moves: the memory is not moved, unmapped, remapped or freed;
/// - the memory is exclusive to the decoder until drop: nothing else reads
///   or writes it, as with a `&mut [u8]`.
///
/// Dropping the value releases the memory; the decoder does not touch it
/// afterwards. The decoder reads no byte it has not written since the
/// stream (or, in LZMA2 and xz, the dictionary reset) began, which is why
/// the contents may be any initialized value.
pub unsafe trait WindowMemory: Send {
    /// The start of the memory. Called once.
    fn as_mut_ptr(&mut self) -> *mut u8;

    /// How many bytes [`WindowMemory::as_mut_ptr`] points at: at least the
    /// `len` asked for. Called once.
    fn len(&self) -> usize;

    /// Whether [`WindowMemory::len`] is zero.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A source of caller memory for decoder dictionaries.
///
/// Asked once for each dictionary the decoder needs that is over the
/// [`WindowBacking`]'s in-RAM limit and at or under its ceiling: once per
/// reader for raw LZMA and LZMA2, and once per block for xz, whose blocks
/// each start a new dictionary.
///
/// # Concurrency
///
/// [`WindowAllocator::allocate`] can be called concurrently, from several
/// threads at once. A threaded xz decode (`XzParallelReader`, or
/// `XzAdaptiveDecoder` with more than one thread) asks once per block in
/// flight, so up to once per worker: with `n` threads, up to `n` windows
/// can be alive at the same time, each dropped when its block has been
/// handed to the caller. A refusal fails the decode; it never makes the
/// decoder fall back to fewer threads or to RAM. A caller who wants one
/// lent window at a time passes one thread.
pub trait WindowAllocator: Send + Sync {
    /// Returns `len` bytes for a dictionary, or the error that stopped it.
    /// The error reaches the reader's caller with its `io::ErrorKind` kept.
    ///
    /// # Errors
    ///
    /// Whatever stops the allocator: no disk space, a failed map, a quota.
    fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>>;
}

/// A [`WindowAllocator`], the in-RAM limit above which it is used, and the
/// ceiling on what it is asked for.
///
/// A dictionary of `in_memory_limit` bytes or fewer is allocated by the
/// decoder itself, as with no backing at all. A bigger one is asked of the
/// allocator, and then counts nothing against the reader's memory limit,
/// which bounds only what the decoder allocates itself. A dictionary over
/// [`WindowBacking::max_len`] is refused with the reader's memory-limit
/// error, and the allocator is never asked for it.
///
/// The size compared, and asked of the allocator, is the dictionary buffer
/// the decoder would allocate. For raw LZMA, LZMA2 and the sequential xz
/// reader that is the stream's dictionary size (for xz, clamped to the
/// block's declared size), rounded up as the LZMA SDK rounds it, to a
/// multiple of 4 KiB, 1 MiB or 4 MiB depending on its size. A threaded xz
/// decode holds each block whole, so its dictionary buffer is the block's
/// uncompressed size from the index.
pub struct WindowBacking {
    // `AssertUnwindSafe` keeps the options and readers that hold a backing
    // `UnwindSafe` and `RefUnwindSafe`, as they are without one; the decoder
    // only ever calls `allocate`, and keeps no state of the allocator's.
    allocator: AssertUnwindSafe<Arc<dyn WindowAllocator>>,
    in_memory_limit: u64,
    max_len: u64,
}

impl Clone for WindowBacking {
    fn clone(&self) -> Self {
        WindowBacking {
            allocator: AssertUnwindSafe(Arc::clone(&self.allocator)),
            in_memory_limit: self.in_memory_limit,
            max_len: self.max_len,
        }
    }
}

impl WindowBacking {
    /// The default ceiling: the largest dictionary buffer an LZMA or LZMA2
    /// stream can ask for, a 4 GiB - 1 dictionary rounded up to 4 GiB.
    pub const DEFAULT_MAX_LEN: u64 = 1 << 32;

    /// Uses `allocator` for every dictionary over `in_memory_limit` bytes and
    /// at or under [`WindowBacking::DEFAULT_MAX_LEN`].
    #[must_use]
    pub fn new(allocator: Arc<dyn WindowAllocator>, in_memory_limit: u64) -> Self {
        WindowBacking {
            allocator: AssertUnwindSafe(allocator),
            in_memory_limit,
            max_len: Self::DEFAULT_MAX_LEN,
        }
    }

    /// Sets the largest window the allocator is asked for. A dictionary
    /// over it is refused with the reader's memory-limit error.
    #[must_use]
    pub fn with_max_len(mut self, max_len: u64) -> Self {
        self.max_len = max_len;
        self
    }

    /// The largest dictionary the decoder allocates in RAM itself.
    #[must_use]
    pub fn in_memory_limit(&self) -> u64 {
        self.in_memory_limit
    }

    /// The largest window the allocator is asked for.
    #[must_use]
    pub fn max_len(&self) -> u64 {
        self.max_len
    }

    /// Whether a dictionary buffer of `len` bytes goes to the allocator.
    pub(crate) fn covers(&self, len: usize) -> bool {
        len as u64 > self.in_memory_limit
    }

    /// Whether the allocator may be asked for `len` bytes: at or under the
    /// ceiling, and small enough to be a slice.
    pub(crate) fn admits(&self, len: usize) -> bool {
        len as u64 <= self.max_len && isize::try_from(len).is_ok()
    }

    /// Asks the allocator for a `len`-byte dictionary and checks what it
    /// gave back against the [`WindowMemory`] contract it can check. A `len`
    /// the backing does not admit is refused without asking; callers check
    /// [`WindowBacking::admits`] first to report the memory-limit error.
    pub(crate) fn allocate(&self, len: usize) -> io::Result<CallerWindow> {
        let failed = |source: io::Error| {
            io::Error::new(
                source.kind(),
                WindowAllocFailed {
                    needed: len as u64,
                    source,
                },
            )
        };
        if !self.admits(len) {
            return Err(failed(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "window over the backing's ceiling",
            )));
        }
        let mut mem = self.allocator.allocate(len).map_err(failed)?;
        let ptr = mem.as_mut_ptr();
        let got = mem.len();
        // A length over `isize::MAX` cannot describe real memory; refused
        // before any slice is built, and `mem` is dropped on the way out.
        if ptr.is_null() || got < len || isize::try_from(got).is_err() {
            return Err(failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "window allocator returned a length that does not fit what was asked",
            )));
        }
        Ok(CallerWindow {
            ptr,
            len,
            _owner: AssertUnwindSafe(mem),
        })
    }
}

impl fmt::Debug for WindowBacking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowBacking")
            .field("in_memory_limit", &self.in_memory_limit)
            .field("max_len", &self.max_len)
            .finish_non_exhaustive()
    }
}

/// The allocator's failure, with the size that was asked for.
#[derive(Debug)]
struct WindowAllocFailed {
    needed: u64,
    source: io::Error,
}

impl fmt::Display for WindowAllocFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "window allocator could not provide a {}-byte dictionary: {}",
            self.needed, self.source
        )
    }
}

impl std::error::Error for WindowAllocFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// A dictionary on caller memory: the pointer and length read once from the
/// [`WindowMemory`], which is kept only to be dropped.
pub(crate) struct CallerWindow {
    ptr: *mut u8,
    len: usize,
    // `AssertUnwindSafe` keeps the decoders `UnwindSafe` and
    // `RefUnwindSafe`, as they are with an owned dictionary: the memory is
    // plain bytes, and what a panic leaves in them is no more broken than
    // what it leaves in a `Vec<u8>`.
    _owner: AssertUnwindSafe<Box<dyn WindowMemory>>,
}

// SAFETY: the pointer is to memory the `WindowMemory` contract makes
// exclusive to this value, and the owner it came from is `Send`. Moving both
// to another thread moves the only access to that memory with them.
unsafe impl Send for CallerWindow {}
// SAFETY: through `&CallerWindow` the crate only reads the bytes (`as_slice`)
// and never touches the owner, so shared references across threads are
// shared reads of initialized memory, as with `&[u8]`.
unsafe impl Sync for CallerWindow {}

impl CallerWindow {
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        // SAFETY: `allocate` checked the pointer is non-null and that the
        // owner has at least `len` bytes; the `WindowMemory` contract makes
        // them initialized, stable and exclusive to this value while the
        // owner, which this value holds, lives. `&self` keeps any `&mut`
        // access out for the slice's life.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as `as_slice`; `&mut self` makes this the only access.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

#[cfg(all(test, feature = "enc"))]
mod tests {
    //! Raw LZMA and LZMA2 decoded on caller memory against the same streams
    //! decoded on the decoder's own. Small enough for Miri, which checks the
    //! caller-memory slices and pointer against the allocation they borrow.

    use std::io::Read;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::vec::Vec;

    use super::*;
    use crate::{Lzma2Reader, LzmaEncProps, LzmaReader, encode_lzma_alone, encode_lzma2};

    /// Heap memory filled with a byte pattern, which counts its drops.
    struct HeapWindow {
        buf: Box<[u8]>,
        drops: Arc<AtomicUsize>,
        /// A length to claim instead of the buffer's.
        claim: Option<usize>,
    }

    // SAFETY: a boxed slice is initialized, never moved by moving the box,
    // and owned by this value alone until it drops.
    unsafe impl WindowMemory for HeapWindow {
        fn as_mut_ptr(&mut self) -> *mut u8 {
            self.buf.as_mut_ptr()
        }
        fn len(&self) -> usize {
            self.claim.unwrap_or(self.buf.len())
        }
    }

    impl Drop for HeapWindow {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Hands out heap windows filled with garbage rather than zeroes, or
    /// fails, or hands back less than asked; records every size asked for.
    #[derive(Default)]
    struct Heap {
        asked: Mutex<Vec<usize>>,
        drops: Arc<AtomicUsize>,
        fail: Option<io::ErrorKind>,
        short: bool,
        /// Claims a length over `isize::MAX` for the memory it returns.
        huge: bool,
    }

    impl WindowAllocator for Heap {
        fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>> {
            self.asked.lock().unwrap().push(len);
            if let Some(kind) = self.fail {
                return Err(io::Error::new(kind, "no room for the window"));
            }
            let len = if self.short { len - 1 } else { len };
            // Not zero, and not one value either: a byte the decoder read
            // before writing would change the output.
            let buf = (0..len)
                .map(|i| (i as u8).wrapping_mul(151) ^ 0xA5)
                .collect();
            Ok(Box::new(HeapWindow {
                buf,
                drops: Arc::clone(&self.drops),
                claim: self.huge.then_some(usize::MAX),
            }))
        }
    }

    impl Heap {
        fn asked(&self) -> Vec<usize> {
            self.asked.lock().unwrap().clone()
        }
        fn drops(&self) -> usize {
            self.drops.load(Ordering::SeqCst)
        }
    }

    fn backing(heap: &Arc<Heap>, in_memory_limit: u64) -> WindowBacking {
        WindowBacking::new(
            Arc::clone(heap) as Arc<dyn WindowAllocator>,
            in_memory_limit,
        )
    }

    /// Three times the 4 KiB dictionary, so the window wraps, with matches
    /// both near and a dictionary back.
    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        let mut x = 0x1234_5678u32;
        while v.len() < 12_288 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if x.is_multiple_of(3) && v.len() > 4000 {
                let back = v.len() - 4000;
                let run: Vec<u8> = v[back..back + 40].to_vec();
                v.extend_from_slice(&run);
            } else {
                v.extend_from_slice(b"window ");
                v.push(x as u8);
            }
        }
        v
    }

    fn props() -> LzmaEncProps {
        LzmaEncProps::new().with_level(1).with_dict_size(4096)
    }

    fn read_all(mut r: impl Read) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(out)
    }

    /// Caller memory leaves the readers and the backing with the auto traits
    /// an owned dictionary gives them.
    #[test]
    fn readers_keep_their_auto_traits() {
        use core::panic::{RefUnwindSafe, UnwindSafe};
        fn check<T: Send + Sync + Unpin + UnwindSafe + RefUnwindSafe>() {}
        check::<LzmaReader<&[u8]>>();
        check::<Lzma2Reader<&[u8]>>();
        check::<crate::LzmaDecoder>();
        check::<crate::Lzma2Decoder>();
        check::<WindowBacking>();
    }

    #[test]
    fn lzma_on_caller_memory_matches_owned_and_releases_it() {
        let data = sample();
        let packed = encode_lzma_alone(&data, &props()).unwrap();
        let owned = read_all(LzmaReader::new(packed.as_slice()).unwrap()).unwrap();
        assert_eq!(owned, data);

        let heap = Arc::new(Heap::default());
        let reader =
            LzmaReader::with_window(packed.as_slice(), u64::MAX, backing(&heap, 0)).unwrap();
        assert_eq!(heap.asked(), [4096]);
        assert_eq!(read_all(reader).unwrap(), data);
        assert_eq!(heap.drops(), 1);

        // Out-of-band properties, as a 7z coder passes them.
        let header = crate::LzmaAloneHeader::parse(packed[..13].try_into().unwrap()).unwrap();
        let reader = LzmaReader::with_props_and_window(
            &packed[13..],
            header.props,
            Some(data.len() as u64),
            u64::MAX,
            backing(&heap, 0),
        )
        .unwrap();
        assert_eq!(read_all(reader).unwrap(), data);
        assert_eq!((heap.asked().len(), heap.drops()), (2, 2));
    }

    #[test]
    fn lzma2_on_caller_memory_matches_owned_and_releases_it() {
        let data = sample();
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        let owned = read_all(Lzma2Reader::new(packed.as_slice(), prop).unwrap()).unwrap();
        assert_eq!(owned, data);

        let heap = Arc::new(Heap::default());
        let reader =
            Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 0)).unwrap();
        assert_eq!(heap.asked(), [4096]);
        assert_eq!(read_all(reader).unwrap(), data);
        assert_eq!(heap.drops(), 1);
    }

    #[test]
    fn a_dictionary_within_the_in_memory_limit_never_asks_the_allocator() {
        let data = sample();
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        let heap = Arc::new(Heap::default());
        let reader =
            Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 4096))
                .unwrap();
        assert_eq!(read_all(reader).unwrap(), data);
        let packed = encode_lzma_alone(&data, &props()).unwrap();
        let reader =
            LzmaReader::with_window(packed.as_slice(), u64::MAX, backing(&heap, 4096)).unwrap();
        assert_eq!(read_all(reader).unwrap(), data);
        assert!(heap.asked().is_empty());
    }

    #[test]
    fn the_memory_limit_counts_only_what_stays_in_ram() {
        let data = sample();
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        let in_ram = Lzma2Reader::<&[u8]>::memory_required(prop).unwrap() - 4096;
        let refused = Lzma2Reader::with_memory_limit(packed.as_slice(), prop, in_ram).err();
        assert_eq!(refused.unwrap().kind(), io::ErrorKind::OutOfMemory);
        let heap = Arc::new(Heap::default());
        let reader =
            Lzma2Reader::with_window(packed.as_slice(), prop, in_ram, backing(&heap, 0)).unwrap();
        assert_eq!(read_all(reader).unwrap(), data);

        let packed = encode_lzma_alone(&data, &props()).unwrap();
        let header = crate::LzmaAloneHeader::parse(packed[..13].try_into().unwrap()).unwrap();
        let in_ram = LzmaReader::<&[u8]>::memory_required(header.props) - 4096;
        assert!(LzmaReader::with_memory_limit(packed.as_slice(), in_ram).is_err());
        let reader = LzmaReader::with_window(packed.as_slice(), in_ram, backing(&heap, 0)).unwrap();
        assert_eq!(read_all(reader).unwrap(), data);
        assert!(LzmaReader::with_window(packed.as_slice(), in_ram - 1, backing(&heap, 0)).is_err());
    }

    #[test]
    fn an_allocator_failure_is_returned_with_its_kind() {
        let (prop, packed) = encode_lzma2(&sample(), &props()).unwrap();
        let heap = Arc::new(Heap {
            fail: Some(io::ErrorKind::StorageFull),
            ..Heap::default()
        });
        let err = Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 0))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert!(err.to_string().contains("4096-byte dictionary"), "{err}");
        let source = std::error::Error::source(err.get_ref().unwrap()).unwrap();
        assert_eq!(source.to_string(), "no room for the window");

        let packed = encode_lzma_alone(&sample(), &props()).unwrap();
        let err = LzmaReader::with_window(packed.as_slice(), u64::MAX, backing(&heap, 0))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    }

    #[test]
    fn memory_shorter_than_asked_is_refused_and_released() {
        let (prop, packed) = encode_lzma2(&sample(), &props()).unwrap();
        let heap = Arc::new(Heap {
            short: true,
            ..Heap::default()
        });
        let err = Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 0))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(heap.drops(), 1);
    }

    #[test]
    fn a_decode_that_fails_midway_releases_the_memory_on_drop() {
        let data = sample();
        let (prop, mut packed) = encode_lzma2(&data, &props()).unwrap();
        let mid = packed.len() / 2;
        packed.truncate(mid);
        let heap = Arc::new(Heap::default());
        let mut reader =
            Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 0)).unwrap();
        let mut out = Vec::new();
        assert!(reader.read_to_end(&mut out).is_err());
        assert!(!out.is_empty() && data.starts_with(&out));
        assert_eq!(heap.drops(), 0);
        drop(reader);
        assert_eq!(heap.drops(), 1);

        let mut packed = encode_lzma_alone(&data, &props()).unwrap();
        for b in &mut packed[200..260] {
            *b = !*b;
        }
        let mut reader =
            LzmaReader::with_window(packed.as_slice(), u64::MAX, backing(&heap, 0)).unwrap();
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        drop(reader);
        assert_eq!((heap.asked().len(), heap.drops()), (2, 2));
    }

    #[test]
    fn a_dictionary_over_the_ceiling_is_refused_without_asking() {
        let data = sample();
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        let heap = Arc::new(Heap::default());
        let capped = || backing(&heap, 0).with_max_len(4095);
        assert_eq!(backing(&heap, 0).max_len(), WindowBacking::DEFAULT_MAX_LEN);
        let err = Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, capped())
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        let lzma = encode_lzma_alone(&data, &props()).unwrap();
        let err = LzmaReader::with_window(lzma.as_slice(), u64::MAX, capped())
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        assert!(heap.asked().is_empty());

        // At the ceiling it is asked.
        let at = backing(&heap, 0).with_max_len(4096);
        let reader = Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, at).unwrap();
        assert_eq!(read_all(reader).unwrap(), data);
        assert_eq!(heap.asked(), [4096]);
    }

    #[test]
    fn a_length_over_isize_max_is_refused_and_released() {
        // Asked for: refused before the allocator is.
        let heap = Arc::new(Heap::default());
        let unbounded = backing(&heap, 0).with_max_len(u64::MAX);
        let too_long = isize::MAX as usize + 1;
        assert!(!unbounded.admits(too_long));
        assert!(unbounded.allocate(too_long).is_err());
        assert!(heap.asked().is_empty());

        // Returned: refused before any slice is built, and released.
        let (prop, packed) = encode_lzma2(&sample(), &props()).unwrap();
        let heap = Arc::new(Heap {
            huge: true,
            ..Heap::default()
        });
        let err = Lzma2Reader::with_window(packed.as_slice(), prop, u64::MAX, backing(&heap, 0))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!((heap.asked().len(), heap.drops()), (1, 1));
    }
}
