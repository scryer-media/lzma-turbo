//! Decoding a stream that is already all there, with every run read at once.
//!
//! [`Lzma2ParallelDecoder::decode`] takes a reader, as 7-Zip's `MtDec` does,
//! and a reader can only be read in order: a thread reads its block's whole
//! input before the next thread may start reading its own. On a stream of
//! many runs, the last run cannot start decoding until every run before it
//! has been read, one after another, and that serial read is what the
//! all-thread wall time waits on.
//!
//! A source that can be read at any offset does not have to be read in
//! order. [`Lzma2ParallelDecoder::decode_at`] finds each run from its chunk
//! headers alone (one small positional read per chunk; payloads are stepped
//! over, never read) as a worker asks for the next one, and the worker then
//! reads and decodes that run itself while the caller's thread writes
//! finished runs in order. Only the runs in flight are held, however many the
//! stream has. The runs, their boundaries and their decoding are the ones
//! [`Lzma2RunScanner`] and the run decoder of [`super::pool`] already use, so
//! the output is the same bytes.
//!
//! C: nothing; 7-Zip's decoder only ever sees a sequential stream.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::mpsc::channel;
use std::sync::{Condvar, Mutex};

use super::Lzma2ParallelDecoder;
use super::mtdec::alloc_buf;
use crate::buffer::DicBuf;
use crate::error::{Error, FinishMode};
use crate::lzma2::Lzma2Decoder;
use crate::lzma2::scan::{Lzma2Run, Lzma2RunScanner};

/// A source that can be read at any offset, from several threads at once.
///
/// Implemented for [`std::fs::File`] on Unix and Windows, and for byte
/// slices. A call reads what it can at `offset` and returns how many bytes it
/// read, zero meaning `offset` is at or past the end; it does not move any
/// cursor that other readers see.
pub trait ReadAt {
    /// Reads into `buf` from `offset`, returning the number of bytes read.
    ///
    /// # Errors
    ///
    /// Any I/O error from the underlying source.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for [u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(rest) = self.get(start..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

#[cfg(unix)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }
}

#[cfg(windows)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }
}

/// Fills `buf` from `offset`, or fails with [`Error::Truncated`] if the
/// source ends first.
fn read_exact_at<S: ReadAt + ?Sized>(src: &S, buf: &mut [u8], offset: u64) -> io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        match src.read_at(&mut buf[done..], offset + done as u64) {
            Ok(0) => return Err(Error::Truncated.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A positional source read in order, for the paths that hand the stream to
/// [`Lzma2ParallelDecoder::decode`].
struct InOrder<'s, S: ?Sized> {
    src: &'s S,
    pos: u64,
    end: u64,
}

impl<S: ReadAt + ?Sized> Read for InOrder<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let room = usize::try_from(self.end - self.pos).unwrap_or(usize::MAX);
        let want = room.min(buf.len());
        if want == 0 {
            return Ok(0);
        }
        let n = self.src.read_at(&mut buf[..want], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// The bytes read per chunk header while scanning. A header is at most six
/// bytes; the rest of the read is payload the scanner steps over.
const SCAN_READ: usize = 64;

/// What a worker is handed: a run to decode, or the offset from which the
/// stream has to be given to [`Lzma2ParallelDecoder::decode`] instead.
enum Claim {
    Run(Lzma2Run),
    HandOver(u64),
}

/// The runs of the stream at `[start, end)`, found from chunk headers alone,
/// one at a time as the workers ask for them.
///
/// Scanning on demand keeps what is held to the runs in flight, whatever the
/// stream holds, and lets the first run start decoding as soon as its own
/// headers have been walked.
struct Feed {
    scanner: Lzma2RunScanner,
    start: u64,
    end: u64,
    /// The next byte to scan.
    at: u64,
    /// Where the run after the last one handed out begins.
    next_start: u64,
    /// The index the next claim gets.
    next_index: usize,
    /// A hand-over has been issued; nothing more is claimed after it.
    closed: bool,
    /// What one run may cost in RAM: its packed and unpacked bytes
    /// together, or only its packed bytes when its output is lent.
    budget: u64,
    /// Where a run's output over the in-RAM limit is lent from.
    window: Option<crate::WindowBacking>,
    /// The longest run whose output is lent:
    /// [`Lzma2Decoder::max_worker_window_len`].
    lent_max: usize,
}

impl Feed {
    fn new(
        start: u64,
        end: u64,
        budget: u64,
        window: Option<crate::WindowBacking>,
        lent_max: usize,
    ) -> Self {
        Feed {
            scanner: Lzma2RunScanner::new(),
            start,
            end,
            at: start,
            next_start: start,
            next_index: 0,
            closed: false,
            budget,
            window,
            lent_max,
        }
    }

    /// Whether `run` can be decoded here: what it holds in RAM is within
    /// the budget, and a lent output is no longer than a worker's window for
    /// this dictionary and within the backing's ceiling. One that is not is
    /// handed to the stream decoder, which needs no more than a ring block
    /// or the dictionary for it, so the stream's own sizes never set how
    /// much is lent.
    fn fits(&self, run: &Lzma2Run) -> bool {
        let lent = usize::try_from(run.unpacked_len).ok().and_then(|len| {
            self.window
                .as_ref()
                .filter(|w| w.covers(len))
                .map(|w| len <= self.lent_max && w.admits(len))
        });
        match lent {
            Some(admitted) => admitted && run.packed_len <= self.budget,
            None => run
                .packed_len
                .checked_add(run.unpacked_len)
                .is_some_and(|b| b <= self.budget),
        }
    }

    /// The next run in stream order, its index, and whether it can be decoded
    /// here; `None` once the stream's end marker has been reached or a
    /// hand-over has been issued.
    fn claim<S: ReadAt + ?Sized>(&mut self, src: &S) -> Option<(usize, Claim)> {
        if self.closed {
            return None;
        }
        let claim = loop {
            if let Some(r) = self.scanner.next_run() {
                let begins = self.start + r.in_offset;
                self.next_start = begins + r.packed_len;
                if r.has_dict_reset && self.fits(&r) {
                    break Claim::Run(r);
                }
                break Claim::HandOver(begins);
            }
            if self.scanner.finished() {
                return None;
            }
            if self.step(src).is_err() {
                break Claim::HandOver(self.next_start);
            }
        };
        if let Claim::HandOver(_) = claim {
            self.closed = true;
        }
        let i = self.next_index;
        self.next_index += 1;
        Some((i, claim))
    }

    /// Walks one chunk header further, skipping payloads by position.
    fn step<S: ReadAt + ?Sized>(&mut self, src: &S) -> Result<(), Error> {
        let skip = self.scanner.payload_remaining();
        if skip != 0 {
            if skip > self.end - self.at {
                return Err(Error::Truncated);
            }
            self.scanner.skip_payload(skip);
            self.at += skip;
            return Ok(());
        }
        let want = usize::try_from(self.end - self.at)
            .unwrap_or(usize::MAX)
            .min(SCAN_READ);
        if want == 0 {
            return Err(Error::Truncated);
        }
        let mut buf = [0u8; SCAN_READ];
        let n = loop {
            match src.read_at(&mut buf[..want], self.at) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(Error::Read),
            }
        };
        if n == 0 {
            return Err(Error::Truncated);
        }
        self.at += self.scanner.feed(&buf[..n])? as u64;
        Ok(())
    }
}

/// Decodes one whole run, `packed`, into `buf`, the first `want` bytes of
/// which are what its headers promise.
///
/// The same checks as the pool's run decoder: the run must be consumed
/// exactly and produce exactly its declared length, or it is corrupt.
fn decode_one(
    dec: &mut Lzma2Decoder,
    buf: DicBuf,
    want: usize,
    packed: &[u8],
) -> Result<DicBuf, Error> {
    let res = dec
        .set_block_buf(buf, want)
        .and_then(|()| dec.decode_block(want, packed, FinishMode::End));
    let produced = dec.dic_pos();
    let out = dec.take_block_buf();
    let (used, _) = res?;
    if used != packed.len() || produced != want {
        return Err(Error::CorruptData);
    }
    Ok(out)
}

/// Lent run buffers whose runs have been written, for the next runs.
///
/// A buffer is put here only once its run is out, and taken or released
/// before a worker asks the allocator, so the windows alive - in flight or
/// here - never number more than the runs in flight, which is at most the
/// worker count.
struct Spare(Mutex<Vec<DicBuf>>);

impl Spare {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DicBuf>> {
        match self.0.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    /// The output buffer for a run of `want` bytes: owned below the
    /// backing's in-RAM limit, exactly as with no backing; above it a
    /// written run's window when one is big enough, or a new one. A window
    /// too small is released before a new one is asked for.
    fn take(
        &self,
        window: Option<&crate::WindowBacking>,
        want: usize,
    ) -> Result<DicBuf, crate::window::Refusal> {
        if !window.is_some_and(|w| w.covers(want)) {
            return Ok(DicBuf::Owned(Vec::new()));
        }
        if let Some(buf) = self.lock().pop()
            && buf.len() >= want
        {
            return Ok(buf);
        }
        Ok(crate::window::lend(window, want)?.map_or_else(DicBuf::default, DicBuf::Caller))
    }

    /// Keeps a written run's window; an owned buffer is released.
    fn give(&self, buf: DicBuf) {
        if buf.is_lent() {
            self.lock().push(buf);
        }
    }
}

/// Why a worker produced no run.
enum Missed {
    /// The stream is to be handed to [`Lzma2ParallelDecoder::decode`] from
    /// this offset.
    HandOver(u64),
    /// The backing could not lend the run's output: the decode fails here,
    /// after every run before it has been written.
    Window(io::Error),
}

/// Where the workers and the writer meet: how many runs have been written,
/// and whether to stop.
struct Window {
    state: Mutex<(usize, bool)>,
    moved: Condvar,
}

impl Window {
    fn lock(&self) -> std::sync::MutexGuard<'_, (usize, bool)> {
        match self.state.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    /// Waits until run `index` is within `ahead` runs of the writer. False if
    /// the decode is stopping.
    fn admit(&self, index: usize, ahead: usize) -> bool {
        let mut g = self.lock();
        while !g.1 && index >= g.0 + ahead {
            g = match self.moved.wait(g) {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
        }
        !g.1
    }

    fn written(&self, n: usize) {
        self.lock().0 = n;
        self.moved.notify_all();
    }

    fn stop(&self) {
        self.lock().1 = true;
        self.moved.notify_all();
    }
}

impl Lzma2ParallelDecoder {
    /// Decodes the raw LZMA2 stream at `offset` in `source` to `out`, reading
    /// every run in parallel, and returns the number of bytes written.
    ///
    /// `len` bounds the stream: it is the packed stream's length when the
    /// container says what it is (a `.7z` pack stream does), and
    /// [`u64::MAX`] when the stream simply ends at its end marker. Bytes
    /// after the end marker are never read.
    ///
    /// The output is the same as [`Lzma2ParallelDecoder::decode`] over the
    /// same bytes. What differs is the read: `decode`
    /// reads its input in order, so a run cannot start decoding until every
    /// run before it has been read; here each worker reads its own run, so
    /// they all start together. The finished runs are written in order from
    /// the calling thread, and at most [`Lzma2ParallelDecoder::threads`] runs
    /// are held at once, the same bound `decode` keeps.
    ///
    /// It hands the stream to `decode` instead, with the same result, where
    /// parallel reading cannot help or cannot be bounded: a plan of one
    /// thread, or, from that run on, a run larger than the plan's per-thread
    /// budget, or, with a [`crate::WindowBacking`], one longer than
    /// [`Lzma2Decoder::max_worker_window_len`]. Every window this call holds
    /// is released before the hand-over. A run that does not decode, or a header that does not scan, is
    /// handed over from its own start after every run before it has been
    /// written, so a damaged stream fails with the error `decode` gives for
    /// that run. Every run before it has been written by then, where `decode`
    /// drops a whole block of runs that holds an error, so a damaged stream
    /// can leave more output here than from `decode`. Either way it is output
    /// the single-threaded [`crate::Lzma2Decoder`] gives too.
    ///
    /// # Errors
    ///
    /// As [`Lzma2ParallelDecoder::decode`], plus any error `source` returns.
    pub fn decode_at<S: ReadAt + Sync + ?Sized, W: Write + Send>(
        &self,
        source: &S,
        offset: u64,
        len: u64,
        out: W,
    ) -> io::Result<u64> {
        let mut out = out;
        let end = offset.saturating_add(len);
        let rest = |from: u64, out: &mut W| {
            self.decode(
                InOrder {
                    src: source,
                    pos: from,
                    end,
                },
                out,
            )
        };

        let workers = self.plan.threads;
        if self.plan.st_only || workers <= 1 {
            return rest(offset, &mut out);
        }

        let lent_max =
            Lzma2Decoder::max_worker_window_len(self.dict_prop).map_err(io::Error::from)?;
        let feed = Mutex::new(Feed::new(
            offset,
            end,
            self.plan.per_thread,
            self.window.clone(),
            lent_max,
        ));
        let spares = Spare(Mutex::new(Vec::new()));
        let lend = self.window.as_ref();
        let window = Window {
            state: Mutex::new((0, false)),
            moved: Condvar::new(),
        };
        // A decoded run, its length and where the stream continues after
        // it, or why there is none.
        type Msg = (usize, Result<(DicBuf, usize, u64), Missed>);
        let (tx, rx) = channel::<Msg>();

        let mut written = 0u64;
        let mut done = 0usize;
        let mut resume = offset;
        let mut hand_over: Option<u64> = None;
        let mut write_err: Option<io::Error> = None;
        let spare = &spares;

        std::thread::scope(|scope| {
            for w in 0..workers {
                let tx = tx.clone();
                let (feed, window) = (&feed, &window);
                let spawned = std::thread::Builder::new()
                    .name(format!("lzma2-at-{w}"))
                    .spawn_scoped(scope, move || {
                        let Ok(mut dec) = Lzma2Decoder::new_probs_only(self.dict_prop) else {
                            window.stop();
                            return;
                        };
                        loop {
                            let claimed = match feed.lock() {
                                Ok(mut f) => f.claim(source),
                                Err(e) => e.into_inner().claim(source),
                            };
                            let Some((i, claim)) = claimed else { return };
                            if !window.admit(i, workers) {
                                return;
                            }
                            let msg = match claim {
                                Claim::HandOver(at) => Err(Missed::HandOver(at)),
                                Claim::Run(run) => {
                                    let begins = offset + run.in_offset;
                                    let next = begins + run.packed_len;
                                    match usize::try_from(run.unpacked_len) {
                                        Err(_) => Err(Missed::HandOver(begins)),
                                        Ok(want) => match spare.take(lend, want) {
                                            Err(r) => Err(Missed::Window(r.into())),
                                            Ok(buf) => usize::try_from(run.packed_len)
                                                .map_err(|_| Error::Alloc)
                                                .and_then(alloc_buf)
                                                .and_then(|mut packed| {
                                                    read_exact_at(source, &mut packed, begins)
                                                        .map_err(|_| Error::Read)?;
                                                    // Owned output is allocated
                                                    // after the input, as before.
                                                    let buf = if buf.is_lent() {
                                                        buf
                                                    } else {
                                                        DicBuf::Owned(alloc_buf(want)?.into_vec())
                                                    };
                                                    decode_one(&mut dec, buf, want, &packed)
                                                })
                                                .map(|buf| (buf, want, next))
                                                .map_err(|_| Missed::HandOver(begins)),
                                        },
                                    }
                                }
                            };
                            if tx.send((i, msg)).is_err() {
                                return;
                            }
                        }
                    });
                if spawned.is_err() {
                    window.stop();
                    break;
                }
            }
            drop(tx);

            // The writer: runs go out in order, as they finish.
            let mut pending = BTreeMap::new();
            'recv: while let Ok((i, msg)) = rx.recv() {
                pending.insert(i, msg);
                while let Some(msg) = pending.remove(&done) {
                    match msg {
                        Ok((buf, len, next)) => {
                            if let Err(e) = out.write_all(&buf[..len]) {
                                write_err = Some(e);
                                break 'recv;
                            }
                            // Released, or kept for the next run, before the
                            // next run is admitted.
                            spare.give(buf);
                            written += len as u64;
                            resume = next;
                            done += 1;
                            window.written(done);
                        }
                        Err(Missed::HandOver(at)) => {
                            hand_over = Some(at);
                            break 'recv;
                        }
                        Err(Missed::Window(e)) => {
                            write_err = Some(e);
                            break 'recv;
                        }
                    }
                }
            }
            window.stop();
        });
        // Every worker has stopped. A run a worker finished after the writer
        // stopped is still in the channel, its window with it: those and the
        // kept windows go back before the stream decoder, which asks for its
        // own, takes over.
        drop(rx);
        drop(spares);

        if let Some(e) = write_err {
            return Err(e);
        }
        if let Some(at) = hand_over {
            return Ok(written + rest(at, &mut out)?);
        }
        let f = match feed.into_inner() {
            Ok(f) => f,
            Err(e) => e.into_inner(),
        };
        if f.scanner.finished() && !f.closed && done == f.next_index {
            return Ok(written);
        }
        // Workers that could not be started or could not build a decoder:
        // everything written so far is out, and the stream decoder takes the
        // rest.
        Ok(written + rest(resume, &mut out)?)
    }
}
