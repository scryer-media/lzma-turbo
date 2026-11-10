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
//!
//! The threaded raw LZMA2 decoders take one through their `with_window`
//! constructors: [`crate::Lzma2ParallelDecoder`] (both `decode` and
//! `decode_at`), [`crate::Lzma2ParallelReader`],
//! [`crate::Lzma2AdaptiveDecoder`], and the positional
//! [`crate::run::Lzma2RunDecoder`]. A worker there writes a whole block or
//! run into its buffer, not only a dictionary's worth, so the limit and the
//! ceiling are compared with that buffer: the ring decoder's block buffer,
//! or the run's unpacked size for `decode_at` and the adaptive decoder.
//!
//! What a worker may be lent is bounded by the dictionary, not by the
//! stream: [`crate::Lzma2Decoder::max_worker_window_len`], four times the
//! dictionary held to between 1 MiB and 256 MiB, or the dictionary itself
//! when that is larger. A 64 MiB dictionary gives 256 MiB, a 256 MiB one
//! 256 MiB, a 1 GiB one 1 GiB and a 1.5 GiB one 1.5 GiB; 7-Zip's threaded
//! encoder cuts its blocks to exactly that size, so each fits. A block or run
//! longer than the bound, or over the ceiling, is never asked for: it is
//! decoded on the single-threaded path, whose buffer is only the dictionary,
//! and only a dictionary over the ceiling fails the decode. So the bytes lent
//! at once are at most the decoder's `max_windows()` times that bound.
//!
//! An allocator's refusal, short memory, and a panic in the allocator fail
//! the decode with a [`WindowAllocError`] holding the allocator's own error:
//! inside the `io::Error` the decoder returns, for
//! [`WindowAllocError::from_io`] to take out, or from the adaptive decoder's
//! `take_window_error`.

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
/// reader for raw LZMA and LZMA2, once per block for xz, whose blocks each
/// start a new dictionary, and once per worker buffer for the threaded LZMA2
/// decoders, each at most
/// [`crate::Lzma2Decoder::max_worker_window_len`] bytes.
///
/// # Concurrency
///
/// [`WindowAllocator::allocate`] can be called concurrently, from several
/// threads at once. A threaded xz decode (`XzParallelReader`, or
/// `XzAdaptiveDecoder` with more than one thread) asks once per block in
/// flight, so up to once per worker: with `n` threads, up to `n` windows
/// can be alive at the same time, each dropped when its block has been
/// handed to the caller. The threaded LZMA2 decoders ask once per worker
/// buffer and keep it for the worker's next block or run, dropping it when
/// the worker retires or the decoder drops. At most the thread count are
/// alive at once for [`crate::Lzma2ParallelDecoder`] and
/// [`crate::Lzma2ParallelReader`], one more for
/// [`crate::Lzma2AdaptiveDecoder`]'s chase, and one for
/// [`crate::run::Lzma2RunDecoder`]; the parallel and adaptive decoders
/// report theirs through `max_windows()`. A refusal fails the decode; it never
/// makes the decoder fall back to fewer threads or to RAM. A caller who
/// wants one lent window at a time passes one thread.
///
/// # Panics
///
/// A panic in `allocate`, or in the memory's `as_mut_ptr` or `len`, is
/// caught and fails the decode as the allocator's error, of kind
/// [`io::ErrorKind::Other`]: it never unwinds through a decoder's threads.
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
                WindowAllocError {
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
        // A panic in the allocator, or in the memory it returns, is caught
        // here and fails the decode as the allocator's error: it would
        // otherwise unwind out of a worker that other workers wait on.
        let asked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut mem = self.allocator.allocate(len)?;
            let ptr = mem.as_mut_ptr();
            let got = mem.len();
            Ok((mem, ptr, got))
        }));
        let (mem, ptr, got) = match asked {
            Ok(r) => r.map_err(failed)?,
            Err(_) => return Err(failed(io::Error::other("window allocator panicked"))),
        };
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

/// Caller memory for a dictionary buffer of `len` bytes, or `None` when the
/// buffer stays in RAM: there is no backing, or `len` is at or under its
/// in-RAM limit. Over the ceiling it is refused without asking the
/// allocator.
///
/// The one place every threaded LZMA2 path chooses where a buffer lives, at
/// the point it allocates it.
pub(crate) fn lend(
    window: Option<&WindowBacking>,
    len: usize,
) -> Result<Option<CallerWindow>, Refusal> {
    let Some(w) = window.filter(|w| w.covers(len)) else {
        return Ok(None);
    };
    if !w.admits(len) {
        return Err(Refusal::OverCeiling {
            needed: len as u64,
            limit: w.max_len,
        });
    }
    match w.allocate(len) {
        Ok(mem) => Ok(Some(mem)),
        Err(source) => Err(Refusal::Alloc {
            needed: len as u64,
            source,
        }),
    }
}

/// Why [`lend`] gave no memory.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// Over [`WindowBacking::max_len`]; the allocator was not asked.
    OverCeiling { needed: u64, limit: u64 },
    /// The allocator's error, or memory it gave that was short or null, as
    /// [`WindowBacking::allocate`] reports it.
    Alloc { needed: u64, source: io::Error },
}

impl Refusal {
    /// As a crate [`crate::Error`], for the decoders whose errors are that:
    /// the memory-limit error over the ceiling, and
    /// [`crate::Error::WindowAlloc`] with the allocator's error class, with
    /// the allocator's own error beside it, which `Error` cannot carry.
    pub(crate) fn into_parts(self) -> (crate::Error, Option<WindowAllocError>) {
        match self {
            Refusal::OverCeiling { needed, limit } => {
                (crate::Error::memory_limit(needed, limit), None)
            }
            Refusal::Alloc { needed, source } => (
                crate::Error::window_alloc(needed, source.kind()),
                WindowAllocError::from_io(source).ok(),
            ),
        }
    }
}

impl From<Refusal> for io::Error {
    /// The shape [`crate::Lzma2Reader::with_window`] reports: the
    /// memory-limit error over the ceiling, and the allocator's own error,
    /// kind kept, otherwise.
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::OverCeiling { needed, limit } => {
                crate::Error::memory_limit(needed, limit).into()
            }
            Refusal::Alloc { source, .. } => source,
        }
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

/// A [`WindowAllocator`]'s failure, with the size it was asked for.
///
/// A decoder that returns an `io::Error` returns this inside it, with the
/// allocator's `io::ErrorKind`; [`WindowAllocError::from_io`] takes it out.
/// [`crate::Lzma2AdaptiveDecoder`], whose errors are [`crate::Error`], hands
/// it back from `take_window_error` after it fails with
/// [`crate::Error::WindowAlloc`]. Memory the allocator returned short, null
/// or too long for a slice, and a panic in the allocator, are reported the
/// same way, with an `InvalidInput` and an `Other` error as the source.
#[derive(Debug)]
pub struct WindowAllocError {
    needed: u64,
    source: io::Error,
}

impl WindowAllocError {
    /// The bytes the allocator was asked for.
    #[must_use]
    pub fn needed(&self) -> u64 {
        self.needed
    }

    /// The allocator's error, as it returned it.
    #[must_use]
    pub fn allocator_error(&self) -> &io::Error {
        &self.source
    }

    /// The allocator's error, as it returned it, by value.
    #[must_use]
    pub fn into_allocator_error(self) -> io::Error {
        self.source
    }

    /// The allocator's failure inside `e`, an error a decoder returned, or
    /// `e` back unchanged when it is some other error.
    ///
    /// # Errors
    ///
    /// Returns `e` when it does not hold a [`WindowAllocError`].
    pub fn from_io(e: io::Error) -> Result<Self, io::Error> {
        if !e.get_ref().is_some_and(|inner| inner.is::<Self>()) {
            return Err(e);
        }
        let kind = e.kind();
        match e.into_inner().map(|inner| inner.downcast::<Self>()) {
            Some(Ok(w)) => Ok(*w),
            Some(Err(inner)) => Err(io::Error::new(kind, inner)),
            None => Err(kind.into()),
        }
    }
}

impl fmt::Display for WindowAllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "window allocator could not provide a {}-byte dictionary: {}",
            self.needed, self.source
        )
    }
}

impl std::error::Error for WindowAllocError {
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
        /// The windows alive, which this one leaves when it drops.
        live: Arc<Mutex<Live>>,
        /// The length it was asked for.
        asked: usize,
    }

    /// The windows alive at once, and the most there have been.
    #[derive(Default)]
    struct Live {
        windows: usize,
        bytes: usize,
        peak: usize,
        peak_bytes: usize,
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
            let mut live = self.live.lock().unwrap();
            live.windows -= 1;
            live.bytes -= self.asked;
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Hands out heap windows filled with garbage rather than zeroes, or
    /// fails, or hands back less than asked; records every size asked for.
    #[derive(Default)]
    struct Heap {
        asked: Mutex<Vec<usize>>,
        drops: Arc<AtomicUsize>,
        live: Arc<Mutex<Live>>,
        fail: Option<io::ErrorKind>,
        /// Fails only the ask with this number, counting from one.
        fail_at: Option<usize>,
        /// Panics on the ask with this number, counting from one.
        panic_at: Option<usize>,
        /// Bytes to hand out beyond what was asked.
        extra: usize,
        short: bool,
        /// Claims a length over `isize::MAX` for the memory it returns.
        huge: bool,
    }

    impl WindowAllocator for Heap {
        fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>> {
            let nth = {
                let mut asked = self.asked.lock().unwrap();
                asked.push(len);
                asked.len()
            };
            if self.panic_at == Some(nth) {
                panic!("the test allocator panics on ask {nth}");
            }
            if let Some(kind) = self.fail {
                return Err(io::Error::new(kind, "no room for the window"));
            }
            if self.fail_at == Some(nth) {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "no room for the window",
                ));
            }
            let got = if self.short {
                len - 1
            } else {
                len + self.extra
            };
            // Not zero, and not one value either: a byte the decoder read
            // before writing would change the output.
            let buf = (0..got)
                .map(|i| (i as u8).wrapping_mul(151) ^ 0xA5)
                .collect();
            {
                let mut live = self.live.lock().unwrap();
                live.windows += 1;
                live.bytes += len;
                live.peak = live.peak.max(live.windows);
                live.peak_bytes = live.peak_bytes.max(live.bytes);
            }
            Ok(Box::new(HeapWindow {
                buf,
                drops: Arc::clone(&self.drops),
                claim: self.huge.then_some(usize::MAX),
                live: Arc::clone(&self.live),
                asked: len,
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
        /// The windows alive now, and the most alive at once so far, in
        /// windows and in bytes asked for.
        fn live(&self) -> (usize, usize, usize) {
            let live = self.live.lock().unwrap();
            (live.windows, live.peak, live.peak_bytes)
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

    // -- the threaded LZMA2 decoders -----------------------------------------

    use crate::{
        DrainStatus, Lzma2AdaptiveDecoder, Lzma2Decoder, Lzma2MtOptions, Lzma2ParallelDecoder,
        Lzma2ParallelReader, encode_lzma2_mt,
    };

    /// The size of each of the stream's runs. Over the ring's 16 KiB
    /// small-block threshold, so `decode` gives each its own block.
    const RUN: usize = 20_480;

    /// Three runs of `RUN` bytes, each starting a new dictionary, with a
    /// 4 KiB dictionary: the shape a threaded decoder can split.
    fn runs() -> (u8, Vec<u8>, Vec<u8>) {
        runs_of(3)
    }

    fn runs_of(n: usize) -> (u8, Vec<u8>, Vec<u8>) {
        let mut data = Vec::new();
        while data.len() < n * RUN {
            data.extend_from_slice(&sample());
        }
        data.truncate(n * RUN);
        let (prop, packed) = encode_lzma2_mt(&data, &props(), RUN as u64, 1).unwrap();
        (prop, packed, data)
    }

    /// The most a worker is lent at once for the 4 KiB test dictionary:
    /// 1 MiB, the smallest ring block.
    fn bound(prop: u8) -> usize {
        let b = Lzma2Decoder::max_worker_window_len(prop).unwrap();
        assert_eq!(b, 1 << 20);
        b
    }

    fn threads(n: usize) -> Lzma2MtOptions {
        Lzma2MtOptions {
            threads: n,
            memory_limit: u64::MAX,
        }
    }

    fn two() -> Lzma2MtOptions {
        threads(2)
    }

    fn ring(prop: u8, heap: &Arc<Heap>, limit: u64) -> Lzma2ParallelDecoder {
        Lzma2ParallelDecoder::with_window(prop, &two(), backing(heap, limit)).unwrap()
    }

    fn decode(d: &Lzma2ParallelDecoder, packed: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        d.decode(packed, &mut out).map(|_| out)
    }

    fn decode_at(d: &Lzma2ParallelDecoder, packed: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        d.decode_at(packed, 0, packed.len() as u64, &mut out)
            .map(|_| out)
    }

    fn reader(prop: u8, packed: &[u8], window: WindowBacking) -> io::Result<Vec<u8>> {
        read_all(Lzma2ParallelReader::with_window(
            io::Cursor::new(packed.to_vec()),
            prop,
            &two(),
            window,
        )?)
    }

    /// Feeds the whole stream and drains it, in order.
    fn adaptive(mut d: Lzma2AdaptiveDecoder, packed: &[u8]) -> Result<Vec<u8>, crate::Error> {
        adaptive_mut(&mut d, packed)
    }

    fn adaptive_mut(d: &mut Lzma2AdaptiveDecoder, packed: &[u8]) -> Result<Vec<u8>, crate::Error> {
        let mut out = Vec::new();
        let mut fed = 0;
        loop {
            if fed < packed.len() {
                fed += d.feed(&packed[fed..])?;
                if fed == packed.len() {
                    d.end_of_input();
                }
            }
            let status = d.drain(|_, b| out.extend_from_slice(b))?;
            if status == DrainStatus::Finished {
                return Ok(out);
            }
        }
    }

    fn adaptive_on(prop: u8, window: WindowBacking) -> Lzma2AdaptiveDecoder {
        Lzma2AdaptiveDecoder::with_window(prop, &two(), window).unwrap()
    }

    /// The threaded paths, by name, as [`on_path`] runs them.
    const PATHS: [&str; 4] = ["decode", "decode_at", "reader", "adaptive"];

    /// Decodes `packed` on `path` with two threads and `window`, and returns
    /// what came out and the most windows that path may hold. An adaptive
    /// [`crate::Error::WindowAlloc`] comes back as the `io::Error` the other
    /// paths give, built from what `take_window_error` hands back.
    fn on_path(
        path: &str,
        prop: u8,
        packed: &[u8],
        window: WindowBacking,
    ) -> (io::Result<Vec<u8>>, usize) {
        let ring = Lzma2ParallelDecoder::with_window(prop, &two(), window.clone()).unwrap();
        match path {
            "decode" => (decode(&ring, packed), ring.max_windows()),
            "decode_at" => (decode_at(&ring, packed), ring.max_windows()),
            "reader" => (reader(prop, packed, window), ring.max_windows()),
            _ => {
                let mut d = adaptive_on(prop, window);
                let max = d.max_windows();
                let got =
                    adaptive_mut(&mut d, packed).map_err(|e| match (&e, d.take_window_error()) {
                        (crate::Error::WindowAlloc { kind, .. }, Some(w)) => {
                            assert_eq!(*kind, w.allocator_error().kind());
                            io::Error::new(*kind, w)
                        }
                        (crate::Error::WindowAlloc { .. }, None) => {
                            panic!("adaptive: {e} with no allocator error to take")
                        }
                        (_, None) => e.into(),
                        (_, Some(w)) => panic!("adaptive: {e} with an allocator error {w}"),
                    });
                (got, max)
            }
        }
    }

    /// Every window went back, and no more were alive at once than `max`,
    /// nor more bytes than `max` times `bound`.
    fn released_within(heap: &Heap, max: usize, bound: usize, ctx: &str) {
        let (live, peak, peak_bytes) = heap.live();
        assert_eq!(live, 0, "{ctx}: a window was kept");
        assert!(peak <= max, "{ctx}: {peak} windows at once, {max} allowed");
        assert!(
            peak_bytes <= max * bound,
            "{ctx}: {peak_bytes} bytes at once, over {max} x {bound}"
        );
        assert!(
            heap.asked().iter().all(|&n| n <= bound),
            "{ctx}: {:?}",
            heap.asked()
        );
    }

    /// Something was lent, every size asked of `heap` was over `limit` and
    /// within `bound`, the windows alive at once stayed within `max`, and
    /// every window went back.
    fn lent_within(heap: &Heap, limit: usize, max: usize, bound: usize, ctx: &str) {
        let asked = heap.asked();
        assert!(!asked.is_empty(), "{ctx}: nothing was lent");
        assert!(asked.iter().all(|&n| n > limit), "{ctx}: {asked:?}");
        assert_eq!(heap.drops(), asked.len(), "{ctx}: a window was kept");
        assert!(heap.live().1 >= 1, "{ctx}");
        released_within(heap, max, bound, ctx);
    }

    /// `err` is the allocator's refusal: its kind, with the allocator's own
    /// error inside, which `from_io` hands back by value.
    fn is_allocator_error(err: io::Error, kind: io::ErrorKind, said: &str, ctx: &str) {
        assert_eq!(err.kind(), kind, "{ctx}: {err}");
        assert!(err.to_string().contains("-byte dictionary"), "{ctx}: {err}");
        let w = WindowAllocError::from_io(err).unwrap_or_else(|e| panic!("{ctx}: {e}"));
        assert!(w.needed() > 0, "{ctx}");
        let source = w.into_allocator_error();
        assert_eq!(source.kind(), kind, "{ctx}");
        assert_eq!(source.to_string(), said, "{ctx}");
    }

    /// Each threaded decoder gives the bytes its owned decode gives, on
    /// garbage-filled caller memory, and releases every window it was lent.
    /// A worker reuses its window: the ring and `decode_at` ask for at most
    /// one per thread for three runs.
    #[test]
    fn threaded_lzma2_on_caller_memory_matches_owned_and_releases_it() {
        let (prop, packed, data) = runs();
        let owned = Lzma2ParallelDecoder::new(prop, &two()).unwrap();
        assert_eq!(decode(&owned, &packed).unwrap(), data);
        assert_eq!(ring(prop, &Arc::default(), 0).max_windows(), 2);
        assert_eq!(
            adaptive_on(prop, backing(&Arc::default(), 0)).max_windows(),
            3
        );

        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert_eq!(got.unwrap(), data, "{path}");
            assert!(heap.asked().len() <= max, "{path}: {:?}", heap.asked());
            if path == "decode_at" {
                assert!(heap.asked().iter().all(|&n| n == RUN), "{:?}", heap.asked());
            }
            lent_within(&heap, 0, max, bound(prop), path);
        }
    }

    /// An allocator may hand back more than it was asked for; the decoders
    /// use what they asked for, and the output is the same.
    #[test]
    fn threaded_lzma2_on_memory_longer_than_asked_matches_owned() {
        let (prop, packed, data) = runs();
        for path in PATHS {
            let heap = Arc::new(Heap {
                extra: 4096,
                ..Heap::default()
            });
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert_eq!(got.unwrap(), data, "{path}");
            lent_within(&heap, 0, max, bound(prop), path);
        }
    }

    /// Runs that grow and then shrink, so a worker's window is reused for a
    /// smaller run and replaced for a bigger one.
    #[test]
    #[cfg_attr(miri, ignore = "decodes 155 KiB; the smaller tests cover the slices")]
    fn threaded_lzma2_on_runs_that_grow_and_shrink() {
        let mut data = Vec::new();
        let mut packed = Vec::new();
        let mut prop = 0;
        for (n, len) in [17_000, 40_000, 20_000, 60_000, 18_000]
            .into_iter()
            .enumerate()
        {
            let mut run: Vec<u8> = sample()
                .into_iter()
                .cycle()
                .skip(n * 97)
                .take(len)
                .collect();
            run[0] = n as u8;
            let (p, mut stream) = encode_lzma2(&run, &props()).unwrap();
            assert_eq!(stream.pop(), Some(0), "the end marker");
            prop = p;
            packed.extend_from_slice(&stream);
            data.extend_from_slice(&run);
        }
        packed.push(0);
        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert_eq!(got.unwrap(), data, "{path}");
            lent_within(&heap, 0, max, bound(prop), path);
        }
    }

    /// At or under the in-RAM limit nothing is lent, and the plan and its
    /// estimate are what they are with no backing.
    #[test]
    fn threaded_lzma2_within_the_in_memory_limit_never_asks_the_allocator() {
        let (prop, packed, data) = runs();
        let heap = Arc::new(Heap::default());
        let owned = Lzma2ParallelDecoder::new(prop, &two()).unwrap();
        let d = ring(prop, &heap, 1 << 20);
        assert_eq!(d.memory_estimate(), owned.memory_estimate());
        assert_eq!(d.threads(), owned.threads());
        for path in PATHS {
            let (got, _) = on_path(path, prop, &packed, backing(&heap, 1 << 20));
            assert_eq!(got.unwrap(), data, "{path}");
        }
        let a = adaptive_on(prop, backing(&heap, 1 << 20));
        assert_eq!(a.dispatch_cost(RUN as u64), RUN as u64);
        assert!(heap.asked().is_empty(), "{:?}", heap.asked());
    }

    /// Lent memory counts nothing against the memory limit: under a limit
    /// that leaves the owned plan one thread, the lent plan keeps the two
    /// asked for, and its estimate is what stays in RAM. The reader adds its
    /// five 1 MiB output pieces.
    #[test]
    fn threaded_lzma2_counts_only_what_stays_in_ram() {
        let (prop, packed, data) = runs();
        // Per thread: the 1 MiB block buffer, its 1 MiB + 64 KiB of input
        // and 320 KiB of overhead. Two threads of everything but the block.
        let ram_per_thread = (1 << 20) + (64 << 10) + (320 << 10);
        let options = Lzma2MtOptions {
            threads: 2,
            memory_limit: 2 * ram_per_thread,
        };
        let owned = Lzma2ParallelDecoder::new(prop, &options).unwrap();
        assert_eq!(owned.threads(), 1);
        let heap = Arc::new(Heap::default());
        let d = Lzma2ParallelDecoder::with_window(prop, &options, backing(&heap, 0)).unwrap();
        assert_eq!(d.threads(), 2);
        // Two workers' input and overhead, and the tail's 1 MiB input
        // buffer; neither the blocks nor the tail's dictionary.
        assert_eq!(d.memory_estimate(), 2 * ram_per_thread + (1 << 20));
        assert_eq!(decode(&d, &packed).unwrap(), data);
        lent_within(&heap, 0, d.max_windows(), bound(prop), "decode");
        let heap = Arc::new(Heap::default());
        assert_eq!(decode_at(&d, &packed).unwrap(), data);
        let r = Lzma2ParallelReader::with_window(
            io::Cursor::new(packed.clone()),
            prop,
            &options,
            backing(&heap, 0),
        )
        .unwrap();
        assert_eq!(r.memory_estimate(), d.memory_estimate() + (5 << 20));
        assert_eq!(read_all(r).unwrap(), data);
        lent_within(
            &heap,
            0,
            2 * d.max_windows(),
            bound(prop),
            "decode_at, reader",
        );
        let r = Lzma2ParallelReader::new(io::Cursor::new(packed.clone()), prop, &options).unwrap();
        assert_eq!(r.memory_estimate(), owned.memory_estimate() + (5 << 20));

        let heap = Arc::new(Heap::default());
        let a = adaptive_on(prop, backing(&heap, 0));
        assert_eq!(a.dispatch_cost(RUN as u64), 0);
        let max = a.max_windows();
        assert_eq!(adaptive(a, &packed).unwrap(), data);
        lent_within(&heap, 0, max, bound(prop), "adaptive");
    }

    /// The adaptive decoder's RAM gauge leaves out lent output; its work
    /// gauge keeps it, as it always has. With no backing the two agree.
    #[test]
    fn adaptive_in_flight_ram_bytes_leaves_out_lent_output() {
        let (prop, packed, data) = runs();
        for limit in [0, u64::MAX] {
            let heap = Arc::new(Heap::default());
            let mut d = adaptive_on(prop, backing(&heap, limit));
            let mut out = Vec::new();
            let mut fed = 0;
            loop {
                if fed < packed.len() {
                    fed += d.feed(&packed[fed..]).unwrap();
                    if fed == packed.len() {
                        d.end_of_input();
                    }
                }
                let status = d.drain(|_, b| out.extend_from_slice(b)).unwrap();
                let (ram, all) = (d.in_flight_ram_bytes(), d.in_flight_bytes());
                if limit == 0 {
                    assert!(ram <= all && (all - ram) % RUN as u64 == 0, "{ram} {all}");
                } else {
                    assert_eq!(ram, all);
                }
                if status == DrainStatus::Finished {
                    break;
                }
            }
            assert_eq!(out, data);
            assert_eq!(heap.asked().is_empty(), limit != 0);
        }
    }

    /// A buffer over the ceiling is never asked for. The threaded decoders
    /// give such a run to the single-threaded path, whose buffer is only the
    /// dictionary; a dictionary over the ceiling fails with the memory-limit
    /// error, still without asking.
    #[test]
    fn threaded_lzma2_over_the_ceiling_is_refused_without_asking() {
        let (prop, packed, data) = runs();
        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let at_dict = backing(&heap, 0).with_max_len(4096);
            let (got, max) = on_path(path, prop, &packed, at_dict);
            assert_eq!(got.unwrap(), data, "{path}");
            assert!(
                heap.asked().iter().all(|&n| n == 4096),
                "{:?}",
                heap.asked()
            );
            lent_within(&heap, 0, max, 4096, path);
        }

        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let under = backing(&heap, 0).with_max_len(4095);
            let err = on_path(path, prop, &packed, under).0.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::OutOfMemory, "{path}: {err}");
            let e = err.get_ref().and_then(|e| e.downcast_ref::<crate::Error>());
            assert!(
                matches!(e, Some(crate::Error::MemoryLimit { limit: 4095, .. })),
                "{path}: {err}"
            );
            assert!(heap.asked().is_empty(), "{path}: {:?}", heap.asked());
        }
    }

    /// An allocator's refusal fails the decode with the allocator's error
    /// kind, with the allocator's own error to take back by value, and the
    /// decode is never retried on fewer threads or in RAM.
    #[test]
    fn threaded_lzma2_allocator_refusal_hands_back_its_error() {
        let (prop, packed, _) = runs();
        for path in PATHS {
            let heap = Arc::new(Heap {
                fail: Some(io::ErrorKind::StorageFull),
                ..Heap::default()
            });
            let err = on_path(path, prop, &packed, backing(&heap, 0))
                .0
                .unwrap_err();
            is_allocator_error(
                err,
                io::ErrorKind::StorageFull,
                "no room for the window",
                path,
            );
            assert!(!heap.asked().is_empty(), "{path}");
        }
    }

    /// The k-th ask refused, for every k up to two past the thread count:
    /// once the refused ask has been made the decode fails with the
    /// allocator's error, and before it, it gives the whole output. Either
    /// way every window goes back, within the bound.
    #[test]
    fn threaded_lzma2_refusal_on_any_ask_fails_and_releases() {
        let (prop, packed, data) = runs();
        for k in 1..=4 {
            for path in PATHS {
                let ctx = format!("{path}, ask {k}");
                let heap = Arc::new(Heap {
                    fail_at: Some(k),
                    ..Heap::default()
                });
                let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
                if heap.asked().len() >= k {
                    let err = got.unwrap_err();
                    is_allocator_error(
                        err,
                        io::ErrorKind::StorageFull,
                        "no room for the window",
                        &ctx,
                    );
                } else {
                    assert_eq!(got.unwrap(), data, "{ctx}");
                }
                released_within(&heap, max, bound(prop), &ctx);
            }
        }
    }

    /// A panic in the allocator fails the decode as the allocator's error,
    /// on every path, and hangs nothing.
    #[test]
    fn threaded_lzma2_allocator_panic_fails_the_decode() {
        let (prop, packed, _) = runs();
        for path in PATHS {
            let heap = Arc::new(Heap {
                panic_at: Some(1),
                ..Heap::default()
            });
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            is_allocator_error(
                got.unwrap_err(),
                io::ErrorKind::Other,
                "window allocator panicked",
                path,
            );
            released_within(&heap, max, bound(prop), path);
        }
    }

    /// A `Write` that fails on its k-th call fails the decode with its own
    /// error, and every window goes back.
    #[test]
    fn threaded_lzma2_output_failure_at_each_write_releases() {
        struct FailAt {
            calls: usize,
            at: usize,
            out: Vec<u8>,
        }
        impl io::Write for FailAt {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == self.at {
                    return Err(io::Error::other("the sink is full"));
                }
                self.out.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (prop, packed, data) = runs();
        for at_path in [false, true] {
            for at in 1.. {
                let ctx = format!("decode_at {at_path}, write {at}");
                let heap = Arc::new(Heap::default());
                let d = ring(prop, &heap, 0);
                let mut sink = FailAt {
                    calls: 0,
                    at,
                    out: Vec::new(),
                };
                let got = if at_path {
                    d.decode_at(packed.as_slice(), 0, packed.len() as u64, &mut sink)
                } else {
                    d.decode(packed.as_slice(), &mut sink)
                };
                assert!(!heap.asked().is_empty(), "{ctx}");
                released_within(&heap, d.max_windows(), bound(prop), &ctx);
                if sink.calls < at {
                    got.unwrap();
                    assert_eq!(sink.out, data, "{ctx}");
                    break;
                }
                let err = got.unwrap_err();
                assert_eq!(err.to_string(), "the sink is full", "{ctx}");
                assert!(data.starts_with(&sink.out), "{ctx}");
            }
        }
    }

    /// Dropped mid-decode, the adaptive decoder and the reader give back
    /// every window: the adaptive decoder with runs out and output landed,
    /// the reader after one read.
    #[test]
    fn threaded_lzma2_dropped_mid_decode_releases() {
        let (prop, packed, data) = runs();
        let heap = Arc::new(Heap::default());
        let mut d = adaptive_on(prop, backing(&heap, 0));
        let max = d.max_windows();
        let mut out = Vec::new();
        let mut fed = 0;
        while out.is_empty() {
            if fed < packed.len() {
                fed += d.feed(&packed[fed..]).unwrap();
                if fed == packed.len() {
                    d.end_of_input();
                }
            }
            d.drain(|_, b| out.extend_from_slice(b)).unwrap();
        }
        assert!(data.starts_with(&out));
        drop(d);
        lent_within(&heap, 0, max, bound(prop), "adaptive");

        let heap = Arc::new(Heap::default());
        let mut r = Lzma2ParallelReader::with_window(
            io::Cursor::new(packed.clone()),
            prop,
            &two(),
            backing(&heap, 0),
        )
        .unwrap();
        let mut one = [0u8; 1];
        assert_eq!(r.read(&mut one).unwrap(), 1);
        assert_eq!(one[0], data[0]);
        drop(r);
        lent_within(
            &heap,
            0,
            ring(prop, &heap, 0).max_windows(),
            bound(prop),
            "reader",
        );
    }

    /// `set_threads` lowered mid-decode, with lent windows kept for the
    /// next runs: the windows past the new count go back, the output is
    /// whole, and no more were alive at once than `max_windows` allowed.
    #[test]
    #[cfg_attr(miri, ignore = "decodes 160 KiB; the smaller tests cover the slices")]
    fn adaptive_set_threads_lowered_mid_decode_releases_spares() {
        let (prop, packed, data) = runs_of(8);
        let heap = Arc::new(Heap::default());
        let mut d =
            Lzma2AdaptiveDecoder::with_window(prop, &threads(4), backing(&heap, 0)).unwrap();
        let max = d.max_windows();
        assert_eq!(max, 5);
        let mut out = Vec::new();
        let mut fed = 0;
        let mut lowered = false;
        loop {
            if fed < packed.len() {
                fed += d.feed(&packed[fed..]).unwrap();
                if fed == packed.len() {
                    d.end_of_input();
                }
            }
            let status = d.drain(|_, b| out.extend_from_slice(b)).unwrap();
            if !lowered && out.len() >= 2 * RUN {
                d.set_threads(1);
                lowered = true;
                assert!(d.max_windows() <= max);
            }
            if status == DrainStatus::Finished {
                break;
            }
        }
        assert!(lowered);
        assert_eq!(out, data);
        drop(d);
        lent_within(&heap, 0, max, bound(prop), "adaptive");
    }

    /// Checksums computed over lent output are those computed over owned.
    #[cfg(feature = "crc")]
    #[test]
    fn threaded_lzma2_checksums_over_caller_memory_match_owned() {
        use crate::{Checksum, ChecksumPlan};
        let (prop, packed, data) = runs();
        let plan = ChecksumPlan::new(Checksum::Crc32).with_split_points([7_000, 30_000]);

        let owned = Lzma2ParallelDecoder::new(prop, &two()).unwrap();
        let (_, want) = owned
            .decode_checksummed(packed.as_slice(), Vec::new(), &plan)
            .unwrap();
        assert!(!want.is_empty());
        let heap = Arc::new(Heap::default());
        let d = ring(prop, &heap, 0);
        let mut out = Vec::new();
        let (_, got) = d
            .decode_checksummed(packed.as_slice(), &mut out, &plan)
            .unwrap();
        assert_eq!((out, got), (data.clone(), want.clone()));
        lent_within(&heap, 0, d.max_windows(), bound(prop), "decode_checksummed");

        let segments = |checks: Vec<crate::BlockChecks>| -> Vec<crate::Segment> {
            checks.into_iter().flat_map(|c| c.segments).collect()
        };
        let mut r = Lzma2ParallelReader::with_checksums(
            io::Cursor::new(packed.clone()),
            prop,
            &two(),
            &plan,
        )
        .unwrap();
        assert_eq!(read_all(&mut r).unwrap(), data);
        let want = segments(r.take_checks());
        let heap = Arc::new(Heap::default());
        let mut r = Lzma2ParallelReader::with_checksums_and_window(
            io::Cursor::new(packed.clone()),
            prop,
            &two(),
            &plan,
            backing(&heap, 0),
        )
        .unwrap();
        assert_eq!(read_all(&mut r).unwrap(), data);
        assert_eq!(segments(r.take_checks()), want);
        drop(r);
        lent_within(&heap, 0, d.max_windows(), bound(prop), "reader");

        let mut owned = Lzma2AdaptiveDecoder::new(prop, &two()).unwrap();
        owned.set_checksum(&plan);
        assert_eq!(adaptive_mut(&mut owned, &packed).unwrap(), data);
        let want = segments(owned.take_checks());
        let heap = Arc::new(Heap::default());
        let mut a = adaptive_on(prop, backing(&heap, 0));
        a.set_checksum(&plan);
        let max = a.max_windows();
        assert_eq!(adaptive_mut(&mut a, &packed).unwrap(), data);
        assert_eq!(segments(a.take_checks()), want);
        drop(a);
        lent_within(&heap, 0, max, bound(prop), "adaptive");
    }

    /// Memory shorter than asked is refused with `InvalidInput` and
    /// released.
    #[test]
    fn threaded_lzma2_short_memory_is_refused_and_released() {
        let (prop, packed, _) = runs();
        for path in PATHS {
            let heap = Arc::new(Heap {
                short: true,
                ..Heap::default()
            });
            let err = on_path(path, prop, &packed, backing(&heap, 0))
                .0
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{path}: {err}");
            assert!(WindowAllocError::from_io(err).is_ok(), "{path}");
            assert!(!heap.asked().is_empty(), "{path}");
            assert_eq!(heap.drops(), heap.asked().len(), "{path}");
            assert_eq!(heap.live().0, 0, "{path}");
        }
    }

    /// A block buffer shorter than the block is an error, not a debug
    /// assertion, and leaves the decoder with nothing to write into.
    #[test]
    fn a_short_block_buffer_is_refused() {
        let (prop, _, _) = runs();
        let mut dec = Lzma2Decoder::new_probs_only(prop).unwrap();
        let buf = crate::buffer::DicBuf::Owned(alloc::vec![0; 10]);
        assert!(matches!(
            dec.set_block_buf(buf, 11),
            Err(crate::Error::InternalFailure)
        ));
        assert_eq!(dec.dic_pos(), 0);
    }

    /// A stream with one run, which nothing can split, decodes on caller
    /// memory too: in one worker's window, or on the single-threaded path's
    /// lent dictionary.
    #[test]
    fn an_unsplittable_stream_decodes_on_caller_memory() {
        let data = sample();
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert_eq!(got.unwrap(), data, "{path}");
            lent_within(&heap, 0, max, bound(prop), path);
        }

        // One thread asked for: the single-threaded decode, on its lent
        // dictionary, and the only window.
        let heap = Arc::new(Heap::default());
        let d = Lzma2ParallelDecoder::with_window(prop, &threads(1), backing(&heap, 0)).unwrap();
        assert_eq!(d.max_windows(), 1);
        assert_eq!(d.memory_estimate(), 1 << 20);
        assert_eq!(decode(&d, &packed).unwrap(), data);
        assert_eq!(heap.asked(), [4096]);
        assert_eq!(heap.drops(), 1);
    }

    /// One run longer than a worker's window for its dictionary goes to no
    /// worker: every path decodes it on the single-threaded path, on a lent
    /// dictionary, and nothing over the bound is asked for, whatever the
    /// run's headers say.
    #[test]
    #[cfg_attr(miri, ignore = "decodes 1.5 MiB; the smaller tests cover the slices")]
    fn a_run_longer_than_a_worker_window_is_never_lent_whole() {
        let data = alloc::vec![0x5Au8; 3 << 19];
        let (prop, packed) = encode_lzma2(&data, &props()).unwrap();
        let mut scanner = crate::Lzma2RunScanner::new();
        scanner.feed(&packed).unwrap();
        assert_eq!(scanner.next_run().unwrap().unpacked_len, data.len() as u64);
        assert!(data.len() > bound(prop));
        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert_eq!(got.unwrap(), data, "{path}");
            assert!(
                heap.asked().iter().all(|&n| n == 4096),
                "{path}: {:?}",
                heap.asked()
            );
            lent_within(&heap, 0, max, bound(prop), path);
        }
    }

    /// A run that does not decode fails the decode, and every window goes
    /// back once the decode, or the decoder, is gone.
    #[test]
    fn a_threaded_decode_that_fails_midway_releases_its_windows() {
        let (prop, mut packed, _) = runs();
        let mid = packed.len() / 2;
        for b in &mut packed[mid..mid + 64] {
            *b = !*b;
        }
        for path in PATHS {
            let heap = Arc::new(Heap::default());
            let (got, max) = on_path(path, prop, &packed, backing(&heap, 0));
            assert!(got.is_err(), "{path}");
            lent_within(&heap, 0, max, bound(prop), path);
        }
    }

    /// A damaged run among many, with every worker busy: `decode_at` hands
    /// the stream over from it, and gives back every window it holds before
    /// the stream decoder asks for its own, so no more than `max_windows`
    /// are alive at once.
    #[test]
    #[cfg_attr(miri, ignore = "decodes 320 KiB; the smaller tests cover the slices")]
    fn decode_at_releases_its_windows_before_the_hand_over() {
        let (prop, mut packed, data) = runs_of(16);
        let mut scanner = crate::Lzma2RunScanner::new();
        scanner.feed(&packed).unwrap();
        let run = (0..9).map(|_| scanner.next_run().unwrap()).last().unwrap();
        let at = (run.in_offset + run.packed_len / 2) as usize;
        for b in &mut packed[at..at + 16] {
            *b = !*b;
        }
        let heap = Arc::new(Heap::default());
        let d = Lzma2ParallelDecoder::with_window(prop, &threads(4), backing(&heap, 0)).unwrap();
        assert_eq!(d.max_windows(), 4);
        let mut out = Vec::new();
        let err = d
            .decode_at(packed.as_slice(), 0, packed.len() as u64, &mut out)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert!(out.len() >= 8 * RUN && data.starts_with(&out));
        lent_within(&heap, 0, d.max_windows(), bound(prop), "decode_at");

        let heap = Arc::new(Heap::default());
        let d = Lzma2ParallelDecoder::with_window(prop, &threads(4), backing(&heap, 0)).unwrap();
        assert!(decode(&d, &packed).is_err());
        lent_within(&heap, 0, d.max_windows(), bound(prop), "decode");
    }

    /// The positional run decoder lends its ring over the in-RAM limit,
    /// counts only what stays in RAM, refuses a ring over the ceiling
    /// without asking, in `memory_required_with_window` as in `with_window`,
    /// and hands back an allocator's error as the threaded paths do.
    #[test]
    fn a_run_decoder_lends_its_ring() {
        use crate::run::Lzma2RunDecoder;
        use core::sync::atomic::AtomicBool;

        let (prop, packed, data) = runs();
        let mut scanner = crate::Lzma2RunScanner::new();
        scanner.feed(&packed).unwrap();
        let run = scanner.next_run().unwrap();
        let input = &packed[..run.packed_len as usize];
        let ring_len = Lzma2Decoder::capped_window_len(prop, RUN as u32).unwrap();
        assert_eq!(ring_len, 4096);

        let heap = Arc::new(Heap::default());
        let all = Lzma2RunDecoder::memory_required(prop, run.unpacked_len).unwrap();
        let ram = Lzma2RunDecoder::memory_required_with_window(
            prop,
            run.unpacked_len,
            &backing(&heap, 0),
        )
        .unwrap();
        assert_eq!(all - ram, ring_len as u64);
        let cancel = AtomicBool::new(false);
        let mut dec = Lzma2RunDecoder::with_window(0, prop, run, ram, backing(&heap, 0)).unwrap();
        assert_eq!(heap.asked(), [ring_len]);
        let mut out = Vec::new();
        let mut sink = |_, b: &[u8]| {
            out.extend_from_slice(b);
            Ok(b.len())
        };
        let mut pos = 0;
        while pos < input.len() {
            pos += dec
                .decode(&input[pos..], usize::MAX, &cancel, &mut sink)
                .unwrap()
                .read;
        }
        let _ = dec.decode(&[], usize::MAX, &cancel, &mut sink).unwrap();
        dec.finish(&cancel).unwrap();
        assert_eq!(out, data[..RUN]);
        assert_eq!(heap.drops(), 1);
        assert!(Lzma2RunDecoder::with_window(0, prop, run, ram - 1, backing(&heap, 0)).is_err());

        let heap = Arc::new(Heap::default());
        let capped = backing(&heap, 0).with_max_len(4095);
        assert!(matches!(
            Lzma2RunDecoder::memory_required_with_window(prop, run.unpacked_len, &capped),
            Err(crate::Error::MemoryLimit { limit: 4095, .. })
        ));
        let err = Lzma2RunDecoder::with_window(0, prop, run, u64::MAX, capped)
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        assert!(matches!(
            err.get_ref().and_then(|e| e.downcast_ref::<crate::Error>()),
            Some(crate::Error::MemoryLimit { limit: 4095, .. })
        ));
        assert!(heap.asked().is_empty());

        for (heap, kind, said) in [
            (
                Heap {
                    fail: Some(io::ErrorKind::StorageFull),
                    ..Heap::default()
                },
                io::ErrorKind::StorageFull,
                "no room for the window",
            ),
            (
                Heap {
                    panic_at: Some(1),
                    ..Heap::default()
                },
                io::ErrorKind::Other,
                "window allocator panicked",
            ),
        ] {
            let heap = Arc::new(heap);
            let err = Lzma2RunDecoder::with_window(0, prop, run, u64::MAX, backing(&heap, 0))
                .err()
                .unwrap();
            is_allocator_error(err, kind, said, "run");
            assert_eq!(heap.asked(), [4096]);
            assert_eq!(heap.live().0, 0);
        }
    }
}
