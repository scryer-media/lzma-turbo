//! Run-boundary discovery: where an LZMA2 stream can be cut.
//!
//! An LZMA2 stream is a series of chunks, and a chunk whose control byte asks
//! for a dictionary reset starts a *run* that decodes without reference to
//! anything before it. Runs are the only parallelism the format has, and they
//! are also the only points at which a decode may be handed from one decoder
//! to another, so both the multi-threaded decoder and a caller deciding how to
//! schedule work need the same answer to "where are they?".
//!
//! This is that answer, and it is cheap: it reads chunk headers and skips
//! chunk payloads, so it costs O(chunks), never decodes, and holds a fixed
//! amount of state no matter how the input is split across calls. It runs the
//! same [`Lzma2Frame`] header state machine as the decoder itself, so it
//! cannot disagree with it about what a control byte means.
//!
//! C: there is no counterpart. `Lzma2Dec_Parse` finds the same boundaries, but
//! only as a side effect of filling a worker's block, and its answer is not
//! visible to the caller.

use alloc::collections::VecDeque;

use crate::error::Error;
use crate::lzma::LzmaProps;
use crate::lzma2::frame::{
    LZMA2_CONTROL_COPY_RESET_DIC, Lzma2Frame, Lzma2State, is_uncompressed_state,
};

/// The most complete runs a scanner, or a decoder fed by one, queues ahead
/// of the runs taken.
///
/// A run costs a few bytes of stream (a stored chunk with a dictionary reset
/// and a one-byte payload is four) and a hundred bytes of [`Lzma2Run`], so a
/// stream of tiny runs would otherwise turn every megabyte fed into tens of
/// megabytes of queued runs. At the cap the scanner stops before the control
/// byte that would close another run, keeps its place, and goes on from it on
/// the next call, once runs have been taken. Four thousand runs ahead is far
/// more than any number of threads can take at once.
pub(crate) const MAX_RUNS_AHEAD: usize = 4096;

/// Makes room in `queue` for one more run: the first time, room for
/// [`MAX_RUNS_AHEAD`] at once, so a queue held to the cap is allocated once
/// and never grows.
///
/// # Errors
///
/// [`Error::Alloc`] if the allocation fails.
pub(crate) fn reserve_run(queue: &mut VecDeque<Lzma2Run>) -> Result<(), Error> {
    if queue.len() < queue.capacity() {
        return Ok(());
    }
    let want = MAX_RUNS_AHEAD.saturating_sub(queue.len()).max(1);
    queue.try_reserve_exact(want).map_err(|_| Error::Alloc)
}

/// Whether a control byte ends the run before it: the end marker, or a chunk
/// that resets the dictionary.
fn starts_run(control: u8) -> bool {
    control == 0 || control >= 0xE0 || control == LZMA2_CONTROL_COPY_RESET_DIC
}

/// One independently decodable region of an LZMA2 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lzma2Run {
    /// Offset of the run's first control byte, from the start of the stream.
    pub in_offset: u64,
    /// The run's length in the compressed stream.
    pub packed_len: u64,
    /// Offset of the run's first decoded byte in the output.
    pub out_offset: u64,
    /// How many bytes the run's chunk headers say it decodes to.
    pub unpacked_len: u64,
    /// Whether the run begins with a dictionary reset, and so is genuinely
    /// independent.
    ///
    /// False only for the first run of a stream that does not begin with one,
    /// which is malformed but is reported rather than rejected here: the
    /// decoder is the thing that decides a stream is bad.
    pub has_dict_reset: bool,
    /// What the run's chunks are, by kind, as their headers declare them.
    pub chunks: Lzma2RunChunks,
}

/// The chunks of one [`Lzma2Run`], counted and sized by kind.
///
/// An LZMA2 chunk is either stored (control byte 1 or 2: its payload is the
/// output, and decoding it is a copy) or LZMA-coded (control byte `0x80` and
/// up). The two cost entirely different amounts to decode, and the ratio of a
/// run's packed size to its unpacked size cannot tell them apart: data that
/// barely compresses is LZMA-coded a shade under its own size, and data that
/// does not compress at all is stored a shade over it. These are the chunk
/// headers' own figures, so the split is exact rather than inferred.
///
/// Every byte of the run is in exactly one kind: `lzma_packed + copy_packed`
/// is the run's [`packed_len`](Lzma2Run::packed_len) and `lzma_unpacked +
/// copy_unpacked` its [`unpacked_len`](Lzma2Run::unpacked_len). Packed sizes
/// include the chunk headers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lzma2RunChunks {
    /// LZMA-coded chunks in the run.
    pub lzma_chunks: u64,
    /// Bytes the LZMA-coded chunks occupy in the stream, headers included.
    pub lzma_packed: u64,
    /// Bytes the LZMA-coded chunks decode to.
    pub lzma_unpacked: u64,
    /// Stored chunks in the run.
    pub copy_chunks: u64,
    /// Bytes the stored chunks occupy in the stream, headers included.
    pub copy_packed: u64,
    /// Bytes the stored chunks decode to, which is their payload.
    pub copy_unpacked: u64,
    /// LZMA-coded chunks that reset the coder state (control byte `0xA0` and
    /// up, so including those that also carry new properties or reset the
    /// dictionary).
    pub state_resets: u64,
    /// LZMA-coded chunks that carry new `lc`/`lp`/`pb` properties (control
    /// byte `0xC0` and up).
    pub prop_resets: u64,
}

impl Lzma2RunChunks {
    /// Chunks of either kind.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.lzma_chunks + self.copy_chunks
    }

    /// Adds one chunk whose header has just been read.
    fn add(&mut self, control: u8, header_len: u64, payload_len: u64, unpacked_len: u64) {
        let packed = header_len + payload_len;
        if is_uncompressed_state(control) {
            self.copy_chunks += 1;
            self.copy_packed += packed;
            self.copy_unpacked += unpacked_len;
        } else {
            self.lzma_chunks += 1;
            self.lzma_packed += packed;
            self.lzma_unpacked += unpacked_len;
            if control >= 0xA0 {
                self.state_resets += 1;
            }
            if control >= 0xC0 {
                self.prop_resets += 1;
            }
        }
    }
}

/// Incremental discovery of the runs in an LZMA2 stream.
///
/// Feed it bytes as they arrive with [`Lzma2RunScanner::feed`] and take
/// finished runs out with [`Lzma2RunScanner::next_run`]. A run is reported
/// once the control byte *after* it has been seen, or once the stream's end
/// marker has: until then its length is not yet known.
#[derive(Debug, Clone)]
pub struct Lzma2RunScanner {
    frame: Lzma2Frame,
    /// Scratch for [`Lzma2State::Prop`], which the shared state machine writes
    /// through. The scanner never decodes, so the values are not used.
    prop: LzmaProps,
    in_pos: u64,
    out_pos: u64,
    /// Bytes of the current chunk's payload still to be skipped.
    data_remaining: u64,
    open: Option<OpenRun>,
    ready: VecDeque<Lzma2Run>,
    finished: bool,
    failed: bool,
}

#[derive(Debug, Clone, Copy)]
struct OpenRun {
    in_offset: u64,
    out_offset: u64,
    has_dict_reset: bool,
    chunks: Lzma2RunChunks,
}

impl Default for Lzma2RunScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Lzma2RunScanner {
    /// A scanner positioned at the start of a stream.
    #[must_use]
    pub fn new() -> Self {
        Lzma2RunScanner {
            frame: Lzma2Frame::new(),
            // Any valid triple will do; the scanner does not decode.
            prop: LzmaProps::new(0, 0, 0, 1 << 12).expect("0/0/0 is in range"),
            in_pos: 0,
            out_pos: 0,
            data_remaining: 0,
            open: None,
            ready: VecDeque::new(),
            finished: false,
            failed: false,
        }
    }

    /// True once the stream's end marker has been read. No further input will
    /// be consumed.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// How many bytes of the stream have been scanned.
    #[must_use]
    pub fn in_position(&self) -> u64 {
        self.in_pos
    }

    /// How many bytes the chunk headers seen so far account for.
    #[must_use]
    pub fn out_position(&self) -> u64 {
        self.out_pos
    }

    /// How many complete runs are waiting to be taken.
    #[must_use]
    pub fn pending_runs(&self) -> usize {
        self.ready.len()
    }

    /// Where the run currently being scanned starts, if there is one.
    ///
    /// A caller keeping fed bytes around for a decoder needs this: nothing
    /// before it will be asked for again.
    #[must_use]
    pub fn open_run_offset(&self) -> Option<u64> {
        self.open.map(|o| o.in_offset)
    }

    /// Bytes of the current chunk's payload still to be walked past.
    ///
    /// The scanner never looks at payload bytes, only at how many there are,
    /// so a caller reading from a seekable source can skip them with a seek
    /// instead of a read: ask this, seek that far, and say so with
    /// [`Lzma2RunScanner::skip_payload`].
    #[must_use]
    pub fn payload_remaining(&self) -> u64 {
        self.data_remaining
    }

    /// Tells the scanner that `n` bytes of the current chunk's payload were
    /// skipped rather than fed, and returns how many it accepted.
    ///
    /// Never more than [`Lzma2RunScanner::payload_remaining`]; a caller that
    /// skipped further than that has skipped a chunk header, which the scanner
    /// cannot recover from and will not pretend to.
    pub fn skip_payload(&mut self, n: u64) -> u64 {
        let take = n.min(self.data_remaining);
        self.in_pos += take;
        self.data_remaining -= take;
        take
    }

    /// Takes the oldest complete run.
    pub fn next_run(&mut self) -> Option<Lzma2Run> {
        self.ready.pop_front()
    }

    /// Scans `data`, which continues the stream where the last call left off,
    /// and returns how many of its bytes were consumed.
    ///
    /// Consumes everything unless the stream ends inside `data`, or 4,096
    /// complete runs are waiting to be taken: then it stops before the
    /// control byte that would close another, and the next call, after
    /// [`Lzma2RunScanner::next_run`] has taken some, goes on from there. Chunk
    /// headers split across calls are carried over, never re-read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CorruptData`] for a control byte the format does not
    /// allow in that position. A scanner that has returned an error will keep
    /// returning it. [`Error::Alloc`] if the queue of runs cannot be
    /// allocated; nothing of `data` is consumed past that point, and the call
    /// can be made again.
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, Error> {
        if self.failed {
            return Err(Error::CorruptData);
        }
        let mut pos = 0usize;
        while pos < data.len() {
            if self.finished {
                break;
            }
            if self.data_remaining != 0 {
                let want = usize::try_from(self.data_remaining)
                    .unwrap_or(usize::MAX)
                    .min(data.len() - pos);
                pos += want;
                self.in_pos += want as u64;
                self.data_remaining -= want as u64;
                continue;
            }

            let b = data[pos];
            if self.frame.state == Lzma2State::Control {
                if self.open.is_some() && starts_run(b) {
                    if self.ready.len() >= MAX_RUNS_AHEAD {
                        break;
                    }
                    reserve_run(&mut self.ready)?;
                }
                if !self.on_control(b) {
                    pos += 1;
                    self.in_pos += 1;
                    break;
                }
            }

            let next = self.frame.update_state(b, &mut self.prop);
            pos += 1;
            self.in_pos += 1;
            self.frame.state = next;

            match next {
                Lzma2State::Error => {
                    self.failed = true;
                    return Err(Error::CorruptData);
                }
                Lzma2State::Data => {
                    let control = self.frame.control;
                    let unpacked = u64::from(self.frame.unpack_size);
                    self.out_pos += unpacked;
                    // A stored chunk's header is the control byte and two size
                    // bytes; an LZMA one adds two packed-size bytes and, when
                    // it carries new properties, one more.
                    let (header_len, payload) = if is_uncompressed_state(control) {
                        (3, unpacked)
                    } else {
                        (
                            5 + u64::from(control & 0x40 != 0),
                            u64::from(self.frame.pack_size),
                        )
                    };
                    self.data_remaining = payload;
                    if let Some(open) = self.open.as_mut() {
                        open.chunks.add(control, header_len, payload, unpacked);
                    }
                    // The decoder decrements `unpackSize` as it produces the
                    // chunk's output, so by the time the next control byte
                    // arrives it is zero, and `LZMA2_STATE_UNPACK0` ORs into
                    // it rather than assigning. Skipping the payload has to
                    // leave the same invariant behind.
                    self.frame.unpack_size = 0;
                    // The header state machine expects the payload to be
                    // consumed and the next byte to be a control byte again.
                    self.frame.state = Lzma2State::Control;
                }
                _ => {}
            }
        }
        Ok(pos)
    }

    /// Handles a control byte before the state machine sees it. Returns false
    /// if it ends the stream.
    fn on_control(&mut self, b: u8) -> bool {
        let starts_run = starts_run(b);
        if starts_run {
            self.close_run();
        }
        if b == 0 {
            self.finished = true;
            self.frame.state = Lzma2State::Finished;
            return false;
        }
        if self.open.is_none() {
            self.open = Some(OpenRun {
                in_offset: self.in_pos,
                out_offset: self.out_pos,
                has_dict_reset: starts_run,
                chunks: Lzma2RunChunks::default(),
            });
        }
        true
    }

    fn close_run(&mut self) {
        if let Some(o) = self.open.take() {
            self.ready.push_back(Lzma2Run {
                in_offset: o.in_offset,
                packed_len: self.in_pos - o.in_offset,
                out_offset: o.out_offset,
                unpacked_len: self.out_pos - o.out_offset,
                has_dict_reset: o.has_dict_reset,
                chunks: o.chunks,
            });
        }
    }
}

/// The runs of an LZMA2 stream in a seekable source, without decoding it.
///
/// Scans from the source's current position to the stream's end marker,
/// seeking past chunk payloads rather than reading them, so the cost is one
/// small read per chunk header and not one pass over the packed bytes. The
/// source's position is restored before returning, so a caller can ask this
/// about a packed range and then decode it.
///
/// This is the question a consumer asks before it commits: how many
/// independently decodable runs are in this range, how big are they, and is
/// there enough there to be worth widening for. [`Lzma2RunScanner`] answers it
/// for bytes as they arrive; this answers it for bytes already on disk.
///
/// `dict_prop` is validated and otherwise unused: finding run boundaries needs
/// no dictionary. It is in the signature so that a caller passing a property
/// byte no decoder would accept finds out here rather than after it has
/// committed to a decode.
///
/// # Errors
///
/// [`Error::UnsupportedProps`] for `dict_prop > 40`
/// ([`std::io::ErrorKind::Unsupported`]), [`Error::CorruptData`] for a stream
/// the scanner rejects ([`std::io::ErrorKind::InvalidData`]),
/// [`Error::Truncated`] for a range that ends before the stream's end marker
/// does ([`std::io::ErrorKind::UnexpectedEof`]), each as the `io::Error`
/// payload, and any error the source itself returns.
#[cfg(feature = "std")]
pub fn run_boundaries<R: std::io::Read + std::io::Seek>(
    mut source: R,
    dict_prop: u8,
) -> std::io::Result<Vec<Lzma2Run>> {
    use std::io::{Error as IoError, SeekFrom};

    if dict_prop > 40 {
        return Err(Error::UnsupportedProps.into());
    }
    let start = source.stream_position()?;
    let end = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(start))?;

    let mut scanner = Lzma2RunScanner::new();
    let mut runs = Vec::new();
    let mut buf = [0u8; 4096];
    let mut at = start;
    let mut failed = None;

    while !scanner.finished() {
        while let Some(r) = scanner.next_run() {
            runs.push(r);
        }
        let skip = scanner.payload_remaining();
        if skip != 0 {
            if skip > end - at {
                failed = Some(IoError::from(Error::Truncated));
                break;
            }
            scanner.skip_payload(skip);
            at += skip;
            source.seek(SeekFrom::Start(at))?;
            continue;
        }
        let want = usize::try_from(end - at)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        if want == 0 {
            failed = Some(IoError::from(Error::Truncated));
            break;
        }
        let n = source.read(&mut buf[..want])?;
        if n == 0 {
            failed = Some(IoError::from(Error::Truncated));
            break;
        }
        match scanner.feed(&buf[..n]) {
            Ok(used) => {
                at += used as u64;
                if used != n {
                    source.seek(SeekFrom::Start(at))?;
                }
            }
            Err(e) => {
                failed = Some(IoError::from(e));
                break;
            }
        }
    }
    while let Some(r) = scanner.next_run() {
        runs.push(r);
    }
    source.seek(SeekFrom::Start(start))?;
    match failed {
        Some(e) => Err(e),
        None => Ok(runs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// An LZMA chunk header and a payload of `pack` filler bytes. The scanner
    /// steps over payloads, so they need not decode.
    fn lzma_chunk(out: &mut Vec<u8>, control: u8, unpack: u32, pack: u32) {
        let u = unpack - 1;
        out.push(control | ((u >> 16) as u8 & 0x1F));
        out.extend_from_slice(&(u as u16).to_be_bytes());
        out.extend_from_slice(&((pack - 1) as u16).to_be_bytes());
        if control >= 0xC0 {
            out.push(0x5D);
        }
        out.extend(core::iter::repeat_n(0xAA, pack as usize));
    }

    fn copy_chunk(out: &mut Vec<u8>, control: u8, len: u16) {
        out.push(control);
        out.extend_from_slice(&(len - 1).to_be_bytes());
        out.extend(core::iter::repeat_n(0x55, usize::from(len)));
    }

    /// `n` runs of one stored byte each, four bytes of stream a run, and the
    /// end marker.
    fn tiny_runs(n: usize) -> Vec<u8> {
        let mut s = Vec::new();
        for _ in 0..n {
            copy_chunk(&mut s, 0x01, 1);
        }
        s.push(0);
        s
    }

    /// Feeds `s` `step` bytes at a time, taking every run after each call.
    fn scan_all(s: &[u8], step: usize) -> (Vec<Lzma2Run>, usize) {
        let mut scanner = Lzma2RunScanner::new();
        let mut runs = Vec::new();
        let mut pos = 0;
        let mut most = 0;
        while !scanner.finished() {
            let end = (pos + step).min(s.len());
            pos += scanner.feed(&s[pos..end]).expect("scan");
            most = most.max(scanner.pending_runs());
            while let Some(r) = scanner.next_run() {
                runs.push(r);
            }
        }
        assert_eq!(pos, s.len());
        (runs, most)
    }

    #[test]
    fn a_stream_of_tiny_runs_queues_no_more_than_the_cap() {
        let n = 3 * MAX_RUNS_AHEAD + 7;
        let s = tiny_runs(n);
        let mut scanner = Lzma2RunScanner::new();
        // One call queues the cap's worth of runs, walks the next one, which
        // stays open, and stops inside the input before the control byte that
        // would close it.
        let used = scanner.feed(&s).expect("scan");
        assert_eq!(scanner.pending_runs(), MAX_RUNS_AHEAD);
        assert_eq!(used, 4 * (MAX_RUNS_AHEAD + 1));
        assert_eq!(scanner.open_run_offset(), Some(4 * MAX_RUNS_AHEAD as u64));
        assert_eq!(scanner.in_position(), used as u64);
        // Fed again without taking any, it takes nothing more.
        assert_eq!(scanner.feed(&s[used..]), Ok(0));
        assert_eq!(scanner.pending_runs(), MAX_RUNS_AHEAD);

        let (runs, most) = scan_all(&s, s.len());
        assert_eq!(most, MAX_RUNS_AHEAD);
        assert_eq!(runs.len(), n);
    }

    #[test]
    fn runs_found_under_the_cap_are_the_runs_found_without_it() {
        let n = 2 * MAX_RUNS_AHEAD + 3;
        let s = tiny_runs(n);
        let (whole, _) = scan_all(&s, s.len());
        let (byte, most) = scan_all(&s, 1);
        // A byte at a time never reaches the cap; all at once does. The runs
        // are the same, at the same offsets.
        assert_eq!(most, 1);
        assert_eq!(whole, byte);
        for (i, r) in whole.iter().enumerate() {
            assert_eq!(r.in_offset, 4 * i as u64);
            assert_eq!(r.packed_len, 4);
            assert_eq!(r.out_offset, i as u64);
            assert_eq!(r.unpacked_len, 1);
            assert!(r.has_dict_reset);
        }
    }

    #[test]
    fn every_chunk_header_shape_is_sized_and_classified() {
        let mut s = Vec::new();
        lzma_chunk(&mut s, 0xE0, 1 << 21, 40_000); // dict reset + props: 6-byte header
        lzma_chunk(&mut s, 0x80, 300_000, 9_000); // no reset: 5
        lzma_chunk(&mut s, 0xA0, 70_000, 65_536); // state reset: 5
        lzma_chunk(&mut s, 0xC0, 1, 1); // state reset + props: 6
        copy_chunk(&mut s, 0x02, 65_535); // stored, no reset: 3
        let first_run = s.len() as u64;
        copy_chunk(&mut s, 0x01, 7); // stored with a dictionary reset: a new run
        s.push(0);

        let mut scanner = Lzma2RunScanner::new();
        assert_eq!(scanner.feed(&s), Ok(s.len()));
        let a = scanner.next_run().expect("first run");
        let b = scanner.next_run().expect("second run");
        assert!(scanner.next_run().is_none());

        assert_eq!(a.packed_len, first_run);
        assert_eq!(
            a.chunks,
            Lzma2RunChunks {
                lzma_chunks: 4,
                lzma_packed: (6 + 40_000) + (5 + 9_000) + (5 + 65_536) + (6 + 1),
                lzma_unpacked: (1 << 21) + 300_000 + 70_000 + 1,
                copy_chunks: 1,
                copy_packed: 3 + 65_535,
                copy_unpacked: 65_535,
                state_resets: 3,
                prop_resets: 2,
            }
        );
        assert_eq!(a.chunks.count(), 5);
        assert_eq!(
            b.chunks,
            Lzma2RunChunks {
                copy_chunks: 1,
                copy_packed: 10,
                copy_unpacked: 7,
                ..Lzma2RunChunks::default()
            }
        );
        assert_eq!(b.chunks.lzma_packed + b.chunks.copy_packed, b.packed_len);
    }
}
