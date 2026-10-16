//! The generic multi-threaded decoding framework.
//!
//! C: `C/MtDec.c` and `C/MtDec.h`.
//!
//! # The shape of it
//!
//! The reference does not have a parser thread, a worker pool and a writer
//! thread. It has one ring of `numThreads` identical threads and two tokens
//! circulating in it: `canRead` and `canWrite`, each an auto-reset event per
//! thread. A thread's iteration is
//!
//! 1. wait for `canRead`; while holding it, pull input from the stream into
//!    its own buffer chain and run the callback's `Parse` over it until a
//!    block boundary is found; then hand `canRead` to the next thread;
//! 2. decode that block into its own output buffer, in parallel with every
//!    other thread doing the same to its own;
//! 3. wait for `canWrite`; write its output; hand `canWrite` on.
//!
//! Because both tokens travel the ring in the same direction, read order and
//! write order are the same order, which is what makes the output correct
//! without a reordering buffer, and the number of blocks in flight is exactly
//! the number of threads, which is what bounds the memory. Thread 0 runs on
//! the caller's stack, as in the C.
//!
//! The port keeps that structure. Where the C relies on the token to make an
//! unsynchronised field safe, the port puts the field behind a mutex that the
//! token holder is the only one able to contend for: the lock is free at
//! runtime and is what proves the exclusivity to the compiler. The two
//! windows are [`ReadState`] (everything the read token protects) and
//! [`WriteState`] (everything the write token protects).
//!
//! Not ported: `ICompressProgress` reporting (`CMtProgress`'s byte counters
//! and the `MTDEC_ProgessStep` throttle), which this crate has no API for.
//! The error and interrupt half of `CMtProgress` is ported, as [`ProgState`],
//! because cancelling later blocks depends on it.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread::Scope;

use crate::error::Error;
use crate::mt::event::Event;

/// Either a decoder error or an error from the caller's streams. The C folds
/// both into `SRes`; the port keeps `std::io::Error` intact so a container
/// layer can tell a short read from corrupt data.
#[derive(Debug)]
pub(crate) enum MtError {
    Lzma(Error),
    Io(io::Error),
}

impl From<Error> for MtError {
    fn from(e: Error) -> Self {
        MtError::Lzma(e)
    }
}

impl From<io::Error> for MtError {
    fn from(e: io::Error) -> Self {
        MtError::Io(e)
    }
}

impl From<MtError> for io::Error {
    fn from(e: MtError) -> Self {
        match e {
            MtError::Io(e) => e,
            MtError::Lzma(e) => e.into(),
        }
    }
}

/// C: `EMtDecParseState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseState {
    /// C: `MTDEC_PARSE_CONTINUE`. Continue this block with more input data.
    Continue,
    /// C: `MTDEC_PARSE_OVERFLOW`. The block does not fit the MT buffers; the
    /// caller must fall back to single-threaded decoding for it.
    Overflow,
    /// C: `MTDEC_PARSE_NEW`. A new independently decodable block starts here.
    New,
    /// C: `MTDEC_PARSE_END`. End of the threaded part of the stream.
    End,
}

/// C: `CMtDecCallbackInfo`.
pub(crate) struct CallbackInfo<'a> {
    // in
    pub(crate) start_call: bool,
    pub(crate) src: &'a [u8],
    /// C: `cc->srcFinished`. LZMA2's parse does not consult it (the chunk
    /// headers say where a block ends), but it is part of the protocol.
    #[allow(dead_code)]
    pub(crate) src_finished: bool,
    // in/out: how much of `src` the parse used.
    pub(crate) src_size: usize,
    // out
    pub(crate) state: ParseState,
    pub(crate) can_create_new_thread: bool,
    pub(crate) out_pos: u64,
    /// C: `me->outProcessed_Parse`. It lives in the LZMA2 callback object but
    /// it is read-token state, so the framework carries it in and out rather
    /// than the coder holding it.
    pub(crate) out_processed_parse: u64,
}

/// What the `Write` callback is given to write through.
pub(crate) struct WriteCtx<'a> {
    pub(crate) out: &'a mut (dyn Write + Send),
    pub(crate) in_processed: &'a mut u64,
    pub(crate) out_processed: &'a mut u64,
}

/// C: `IMtDecCallback2`, with the per-coder state folded into the object
/// rather than reached through `coderIndex`: one `Coder` belongs to one
/// thread, so `&mut self` states what `me->coders[coderIndex]` only implied.
pub(crate) trait Coder: Send {
    /// C: `IMtDecCallback2::Parse`.
    fn parse(&mut self, ci: &mut CallbackInfo<'_>);
    /// C: `IMtDecCallback2::PreCode`.
    fn pre_code(&mut self) -> Result<(), MtError>;
    /// C: `IMtDecCallback2::Code`.
    fn code(
        &mut self,
        src: &[u8],
        src_finished: bool,
        in_code_pos: &mut u64,
        out_code_pos: &mut u64,
        stop: &mut bool,
    ) -> Result<(), MtError>;
    /// C: nothing. Called on the worker thread once its block is decoded and
    /// before it waits for the write token, so that anything derived from the
    /// block's bytes is computed in parallel rather than in the ring's one
    /// serialised section. Default: do nothing.
    #[cfg(feature = "crc")]
    fn checksum(&mut self) {}
    /// C: `IMtDecCallback2::Write`.
    fn write(
        &mut self,
        ctx: WriteCtx<'_>,
        need_write_to_stream: bool,
        need_continue: &mut bool,
        can_recode: &mut bool,
    ) -> Result<(), MtError>;
}

/// One input buffer handed back for single-threaded replay, with the number
/// of bytes in it that belong to the stream.
pub(crate) struct ReplayBuf {
    pub(crate) buf: Box<[u8]>,
    pub(crate) len: usize,
}

/// C: `p->threads[i]`, minus everything the thread body now owns.
struct Slot {
    can_read: Event,
    can_write: Event,
}

/// Everything the `canRead` token protects.
struct ReadState<'e> {
    stream: &'e mut (dyn Read + Send),
    /// C: `p->crossBlock` / `crossStart` / `crossEnd`.
    cross: Vec<u8>,
    cross_start: usize,
    cross_end: usize,
    read_was_finished: bool,
    read_res: Option<io::Error>,
    block_index: u64,
    num_started_threads: usize,
    num_started_threads_limit: usize,
    /// C: `CLzma2DecMt::outProcessed_Parse`.
    out_processed_parse: u64,
}

/// Everything the `canWrite` token protects.
struct WriteState<'e> {
    out: &'e mut (dyn Write + Send),
    was_interrupted: bool,
    /// C: `p->codeRes`. See [`MtDec::run`] for why the port returns it where
    /// the C drops it.
    code_res: Option<Error>,
    in_processed: u64,
    out_processed: u64,
    /// C: the `t->inDataSize_Start` / `t->inDataSize` bookkeeping that
    /// `MtDec_Read` later walks, resolved eagerly into the slices it would
    /// have handed back.
    replay: VecDeque<ReplayBuf>,
    num_filled_threads: usize,
    write_err: Option<io::Error>,
}

/// C: the error half of `CMtProgress`, plus `needInterrupt`/`interruptIndex`.
struct ProgState {
    res: Option<Error>,
    need_interrupt: bool,
    interrupt_index: u64,
}

/// What a threaded run left behind for the single-threaded tail.
pub(crate) struct RunResult {
    /// Input already read but not decoded, in stream order. Non-empty exactly
    /// when the threaded pass could not finish the stream, which is C's
    /// `p->needContinue`.
    pub(crate) replay: VecDeque<ReplayBuf>,
    pub(crate) read_was_finished: bool,
    pub(crate) read_res: Option<io::Error>,
    /// C: `p->inProcessed`. Reported for progress; the decode's result is the
    /// output count.
    #[allow(dead_code)]
    pub(crate) in_processed: u64,
    pub(crate) out_processed: u64,
}

/// C: `CMtDec`.
pub(crate) struct MtDec<'e, C> {
    /// C: `p->inBufSize`.
    in_buf_size: usize,
    make_coder: &'e (dyn Fn() -> Result<C, Error> + Sync),
    slots: Vec<Slot>,
    read: Mutex<ReadState<'e>>,
    write: Mutex<WriteState<'e>>,
    prog: Mutex<ProgState>,
    exit_thread: AtomicBool,
}

/// C: `SeqInStream_ReadMax`: read until the buffer is full or the stream ends.
fn read_max(stream: &mut (dyn Read + Send), buf: &mut [u8]) -> (usize, io::Result<()>) {
    let mut filled = 0usize;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return (filled, Err(e)),
        }
    }
    (filled, Ok(()))
}

fn alloc_buf(size: usize) -> Result<Box<[u8]>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(size).map_err(|_| Error::Alloc)?;
    v.resize(size, 0u8);
    Ok(v.into_boxed_slice())
}

impl<'e, C: Coder> MtDec<'e, C> {
    /// C: `MtDec_Construct` plus the assignments `Lzma2DecMt_Decode` makes
    /// into `p->mtc` before calling `MtDec_Code`.
    pub(crate) fn new(
        in_buf_size: usize,
        num_threads_max: usize,
        stream: &'e mut (dyn Read + Send),
        out: &'e mut (dyn Write + Send),
        make_coder: &'e (dyn Fn() -> Result<C, Error> + Sync),
    ) -> Self {
        let num_threads_max = num_threads_max.max(1);
        let mut slots = Vec::with_capacity(num_threads_max);
        for _ in 0..num_threads_max {
            slots.push(Slot {
                can_read: Event::new(),
                can_write: Event::new(),
            });
        }
        MtDec {
            in_buf_size,
            make_coder,
            slots,
            read: Mutex::new(ReadState {
                stream,
                cross: Vec::new(),
                cross_start: 0,
                cross_end: 0,
                read_was_finished: false,
                read_res: None,
                // C: "it must be larger than not_defined index (0)".
                block_index: 1,
                num_started_threads: 0,
                num_started_threads_limit: num_threads_max,
                out_processed_parse: 0,
            }),
            write: Mutex::new(WriteState {
                out,
                was_interrupted: false,
                code_res: None,
                in_processed: 0,
                out_processed: 0,
                replay: VecDeque::new(),
                num_filled_threads: 0,
                write_err: None,
            }),
            prog: Mutex::new(ProgState {
                res: None,
                need_interrupt: false,
                interrupt_index: u64::MAX,
            }),
            exit_thread: AtomicBool::new(false),
        }
    }

    fn lock_read(&self) -> MutexGuard<'_, ReadState<'e>> {
        self.read.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_write(&self) -> MutexGuard<'_, WriteState<'e>> {
        self.write.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// C: `MtDec_GetError_Spec`.
    fn get_error_spec(&self, interrupt_index: u64) -> (Option<Error>, bool) {
        let g = self.prog.lock().unwrap_or_else(|e| e.into_inner());
        (
            g.res,
            g.need_interrupt && interrupt_index > g.interrupt_index,
        )
    }

    /// C: `MtDec_Interrupt`.
    fn interrupt(&self, interrupt_index: u64) {
        let mut g = self.prog.lock().unwrap_or_else(|e| e.into_inner());
        if !g.need_interrupt || interrupt_index < g.interrupt_index {
            g.interrupt_index = interrupt_index;
            g.need_interrupt = true;
        }
    }

    /// C: `MtProgress_SetError`.
    fn set_error(&self, e: Error) {
        let mut g = self.prog.lock().unwrap_or_else(|e| e.into_inner());
        if g.res.is_none() {
            g.res = Some(e);
        }
    }

    /// C: `MtDec_Code`. Runs the ring to completion with the calling thread as
    /// thread 0, and returns what a single-threaded tail would need.
    ///
    /// # Deviation from the C
    ///
    /// `Lzma2DecMt_Decode` never reads `p->mtc.codeRes`, so in the reference a
    /// worker's `SZ_ERROR_DATA` cancels the remaining blocks but is not
    /// returned to the caller: only the container's CRC catches it. This port
    /// returns it, and does not write the partial output of the block that
    /// failed. Decoding corrupt input to a short, successful-looking result is
    /// not acceptable in a library whose caller may have no checksum.
    pub(crate) fn run(&self) -> Result<RunResult, MtError> {
        std::thread::scope(|scope| -> Result<(), MtError> {
            {
                let mut rd = self.lock_read();
                rd.num_started_threads = 1;
            }
            self.slots[0].can_write.set();
            self.slots[0].can_read.set();
            let mut coder = None;
            let mut bufs: Vec<Box<[u8]>> = Vec::new();
            let r = self.thread_func(0, scope, &mut coder, &mut bufs);
            // C leaves the worker threads parked on `canRead` until
            // `MtDec_Destruct`; a scope has to join them here, so release
            // every one of them first. Each thread waits on exactly one of its
            // two events at a time, and both are set, so all of them wake.
            self.exit_thread.store(true, Ordering::Release);
            for s in &self.slots {
                s.can_read.set();
                s.can_write.set();
            }
            r
        })?;

        let mut wr = self.lock_write();
        if let Some(e) = wr.write_err.take() {
            return Err(MtError::Io(e));
        }
        if let Some(e) = wr.code_res {
            return Err(MtError::Lzma(e));
        }
        {
            let prog = self.prog.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = prog.res {
                return Err(MtError::Lzma(e));
            }
        }

        let mut rd = self.lock_read();
        // C: `MtDec_PrepareRead` hands back whatever is left in the cross
        // block after the filled threads' buffers.
        let cross_size = rd.cross_end - rd.cross_start;
        if cross_size != 0 {
            let start = rd.cross_start;
            let buf = rd.cross[start..start + cross_size]
                .to_vec()
                .into_boxed_slice();
            wr.replay.push_back(ReplayBuf {
                len: cross_size,
                buf,
            });
            rd.cross_start = 0;
            rd.cross_end = 0;
        }

        Ok(RunResult {
            replay: std::mem::take(&mut wr.replay),
            read_was_finished: rd.read_was_finished,
            read_res: rd.read_res.take(),
            in_processed: wr.in_processed,
            out_processed: wr.out_processed,
        })
    }

    /// C: `MtDec_ThreadFunc2`.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn thread_func<'s>(
        &'s self,
        index: usize,
        scope: &'s Scope<'s, '_>,
        coder: &mut Option<C>,
        bufs: &mut Vec<Box<[u8]>>,
    ) -> Result<(), MtError>
    where
        C: 's,
        'e: 's,
    {
        loop {
            self.slots[index].can_read.wait();
            if self.exit_thread.load(Ordering::Acquire) {
                return Ok(());
            }

            let mut need_code = false;
            let mut need_write = false;
            let mut is_alloc_error = false;
            let mut overflow = false;
            let mut threading_error = false;
            let mut in_data_size_start = 0usize;
            let mut in_data_size = 0u64;
            let mut can_create_new_thread = false;
            let mut code_res: Option<Error> = None;
            let mut io_err: Option<io::Error> = None;

            let mut rd = self.lock_read();
            let block_index = rd.block_index;
            rd.block_index += 1;

            // C: `res` and `wasInterrupted` are locals; a non-OK `res` here
            // only makes the thread stop taking new work, it is never returned.
            let (mut res, mut was_interrupted) = self.get_error_spec(block_index);
            let mut finish = rd.read_was_finished;

            if res.is_none() && !was_interrupted {
                let mut link = 0usize; // C: the position in the buffer chain
                let mut have_prev = false;
                let mut cross_size = rd.cross_end - rd.cross_start;

                loop {
                    if link == bufs.len() {
                        match alloc_buf(self.in_buf_size) {
                            Ok(b) => bufs.push(b),
                            Err(_) => {
                                finish = true;
                                is_alloc_error = true;
                                break;
                            }
                        }
                    }

                    // C: `parseData` is the cross block on the first pass of a
                    // block that starts with carried-over bytes, and the
                    // thread's own buffer otherwise.
                    let size;
                    let parse_from_cross = cross_size != 0;
                    if parse_from_cross {
                        in_data_size = cross_size as u64;
                        in_data_size_start = cross_size;
                        size = cross_size;
                    } else {
                        let (n, r) = read_max(&mut *rd.stream, &mut bufs[link]);
                        size = n;
                        in_data_size += n as u64;
                        if !have_prev {
                            in_data_size_start = n;
                        }
                        finish = n != self.in_buf_size;
                        if finish {
                            rd.read_was_finished = true;
                        }
                        if let Err(e) = r {
                            // C: "we want to decode all data before error".
                            if rd.read_res.is_none() {
                                rd.read_res = Some(e);
                            }
                            rd.read_was_finished = true;
                            finish = true;
                        }
                    }

                    if coder.is_none() {
                        match (self.make_coder)() {
                            Ok(c) => *coder = Some(c),
                            Err(_) => {
                                finish = true;
                                is_alloc_error = true;
                                break;
                            }
                        }
                    }

                    let parse_state;
                    let parse_src_size;
                    {
                        let cross_from = rd.cross_start;
                        let mut ci = CallbackInfo {
                            start_call: !have_prev,
                            src: if parse_from_cross {
                                &rd.cross[cross_from..cross_from + size]
                            } else {
                                &bufs[link][..size]
                            },
                            src_finished: finish,
                            src_size: 0,
                            state: ParseState::Continue,
                            can_create_new_thread: true,
                            out_pos: 0,
                            out_processed_parse: rd.out_processed_parse,
                        };
                        coder.as_mut().expect("coder created above").parse(&mut ci);
                        parse_state = ci.state;
                        parse_src_size = ci.src_size;
                        can_create_new_thread = ci.can_create_new_thread;
                        rd.out_processed_parse = ci.out_processed_parse;
                        need_write = true;
                    }

                    if parse_state == ParseState::Overflow {
                        // Overflow - switch from MT decoding to ST decoding.
                        finish = true;
                        overflow = true;
                        if parse_from_cross {
                            let from = rd.cross_start;
                            bufs[link][..size].copy_from_slice(&rd.cross[from..from + size]);
                        }
                        rd.cross_start = 0;
                        rd.cross_end = 0;
                        break;
                    }

                    if parse_from_cross {
                        let from = rd.cross_start;
                        bufs[link][..parse_src_size]
                            .copy_from_slice(&rd.cross[from..from + parse_src_size]);
                        rd.cross_start += parse_src_size;
                    }

                    if parse_state != ParseState::Continue || finish {
                        // We don't need to parse in the current thread anymore.
                        if parse_state == ParseState::End {
                            finish = true;
                        }
                        need_code = true;

                        if parse_src_size == size {
                            // Fully parsed - no cross transfer.
                            rd.cross_start = 0;
                            rd.cross_end = 0;
                            break;
                        }

                        if parse_state == ParseState::End {
                            // C also hands the bytes after the end marker to
                            // `Write` as `afterEndData`; the LZMA2 callback
                            // ignores them and nothing else in this crate has
                            // a use for them, so only the accounting is kept.
                            let after_end = size - parse_src_size;
                            in_data_size -= after_end as u64;
                            if !have_prev {
                                in_data_size_start = parse_src_size;
                            }
                            break;
                        }

                        // Partially parsed: the tail belongs to the next block
                        // and has to cross over to the next thread.
                        if parse_from_cross {
                            in_data_size = parse_src_size as u64;
                        } else {
                            if rd.cross.len() != self.in_buf_size {
                                match alloc_buf(self.in_buf_size) {
                                    Ok(b) => rd.cross = b.into_vec(),
                                    Err(_) => {
                                        finish = true;
                                        is_alloc_error = true;
                                        break;
                                    }
                                }
                            }
                            let cr_size = size - parse_src_size;
                            in_data_size -= cr_size as u64;
                            rd.cross[..cr_size].copy_from_slice(&bufs[link][parse_src_size..size]);
                            rd.cross_end = cr_size;
                            rd.cross_start = 0;
                        }

                        if !have_prev {
                            in_data_size_start = parse_src_size;
                        }
                        finish = false;
                        break;
                    }

                    if parse_src_size != size {
                        res = Some(Error::InternalFailure);
                        break;
                    }

                    have_prev = true;
                    link += 1;

                    if cross_size != 0 {
                        cross_size = 0;
                        rd.cross_start = 0;
                        rd.cross_end = 0;
                    }
                }

                if res.is_none() {
                    let (e, w) = self.get_error_spec(block_index);
                    res = e;
                    was_interrupted = w;
                }
            }

            if res.is_none()
                && need_code
                && !was_interrupted
                && let Some(c) = coder.as_mut()
            {
                match c.pre_code() {
                    Ok(()) => {}
                    Err(MtError::Lzma(e)) => {
                        code_res = Some(e);
                        need_code = false;
                        finish = true;
                        if e == Error::Alloc {
                            is_alloc_error = true;
                        }
                    }
                    Err(MtError::Io(e)) => {
                        io_err = Some(e);
                        need_code = false;
                        finish = true;
                    }
                }
            }

            if res.is_some() || was_interrupted {
                finish = true;
            }

            let mut next_thread = None;
            if !finish {
                if rd.num_started_threads < rd.num_started_threads_limit && can_create_new_thread {
                    let new_index = rd.num_started_threads;
                    if self.spawn(new_index, scope).is_ok() {
                        rd.num_started_threads += 1;
                    } else if rd.num_started_threads == 1 {
                        // If only one thread is possible, we leave the
                        // multi-threading code; the block's input is replayed
                        // single-threaded.
                        finish = true;
                        need_code = false;
                        threading_error = true;
                    } else {
                        rd.num_started_threads_limit = rd.num_started_threads;
                    }
                }

                if !finish {
                    let next = index + 1;
                    next_thread = Some(if next >= rd.num_started_threads {
                        0
                    } else {
                        next
                    });
                }
            }

            // Passing the read token. Everything above needed exclusive access
            // to the stream and the cross block; nothing below does.
            drop(rd);

            if let Some(n) = next_thread {
                self.slots[n].can_read.set();
            }

            // ---------- CODE ----------

            let mut in_code_pos = 0u64;
            let mut out_code_pos = 0u64;

            // `was_interrupted` gates this as well as the pre-code above: an
            // interrupted block never had its buffer installed as the
            // decoder's dictionary, so coding it would decode into an empty
            // one - a panic in the portable loop and a write through a
            // dangling pointer in the assembly one. C guards `PreCode` and
            // the code loop with the same `wasInterrupted`.
            if res.is_none()
                && need_code
                && !was_interrupted
                && code_res.is_none()
                && io_err.is_none()
            {
                let c = coder.as_mut().expect("a coder exists once parse has run");
                let mut is_start_block = true;
                let mut link = 0usize;
                loop {
                    let in_size = if is_start_block {
                        in_data_size_start
                    } else {
                        let rem = in_data_size - in_code_pos;
                        self.in_buf_size.min(rem as usize)
                    };

                    in_code_pos += in_size as u64;
                    let src_finished = in_code_pos == in_data_size;
                    let mut stop = true;

                    let r = c.code(
                        &bufs[link][..in_size],
                        src_finished,
                        &mut in_code_pos,
                        &mut out_code_pos,
                        &mut stop,
                    );
                    match r {
                        Ok(()) => {}
                        Err(MtError::Lzma(e)) => {
                            code_res = Some(e);
                            // We interrupt only later blocks.
                            self.interrupt(block_index);
                            break;
                        }
                        Err(MtError::Io(e)) => {
                            io_err = Some(e);
                            self.interrupt(block_index);
                            break;
                        }
                    }

                    if stop || in_code_pos == in_data_size {
                        break;
                    }

                    let (e, w) = self.get_error_spec(block_index);
                    if e.is_some() || w {
                        res = e;
                        was_interrupted = w;
                        break;
                    }

                    link += 1;
                    is_start_block = false;
                }
            }

            // ---------- CHECKSUM ----------
            //
            // Still on the worker thread, still outside the write token.
            #[cfg(feature = "crc")]
            if code_res.is_none()
                && io_err.is_none()
                && res.is_none()
                && !was_interrupted
                && let Some(c) = coder.as_mut()
            {
                c.checksum();
            }

            // ---------- WRITE ----------

            self.slots[index].can_write.wait();
            if self.exit_thread.load(Ordering::Acquire) {
                return Ok(());
            }

            let need_continue;
            {
                let mut wr = self.lock_write();
                let mut is_error_mode = false;
                let mut can_recode = true;
                let mut need_write_to_stream = need_write;

                if wr.was_interrupted {
                    was_interrupted = true;
                } else {
                    if let Some(e) = code_res {
                        wr.was_interrupted = true;
                        if wr.code_res.is_none() {
                            wr.code_res = Some(e);
                        }
                        if e == Error::Alloc {
                            is_alloc_error = true;
                        }
                    }
                    if io_err.is_some() {
                        wr.was_interrupted = true;
                        if wr.write_err.is_none() {
                            wr.write_err = io_err.take();
                        }
                        need_write_to_stream = false;
                    }
                    if threading_error || is_alloc_error || overflow {
                        wr.was_interrupted = true;
                        need_write_to_stream = false;
                    }
                }

                let mut nc = !finish;

                if need_write {
                    let write_to_stream = res.is_none()
                        && need_write_to_stream
                        && !was_interrupted
                        && code_res.is_none();
                    let write_res = match coder.as_mut() {
                        Some(c) => {
                            let WriteState {
                                out,
                                in_processed,
                                out_processed,
                                ..
                            } = &mut *wr;
                            c.write(
                                WriteCtx {
                                    out: &mut **out,
                                    in_processed,
                                    out_processed,
                                },
                                write_to_stream,
                                &mut nc,
                                &mut can_recode,
                            )
                        }
                        None => Ok(()),
                    };

                    match write_res {
                        Ok(()) => {}
                        Err(e) => {
                            match e {
                                MtError::Io(e) => {
                                    if wr.write_err.is_none() {
                                        wr.write_err = Some(e);
                                    }
                                }
                                MtError::Lzma(e) => {
                                    if wr.code_res.is_none() {
                                        wr.code_res = Some(e);
                                    }
                                }
                            }
                            is_error_mode = true;
                            wr.was_interrupted = true;
                            self.interrupt(block_index);
                        }
                    }
                    if !nc && !finish {
                        self.interrupt(block_index);
                    }
                }
                need_continue = nc;

                if can_recode
                    && (!need_code
                        || res.is_some()
                        || wr.was_interrupted
                        || code_res.is_some()
                        || was_interrupted
                        || wr.num_filled_threads != 0
                        || is_error_mode)
                    && (in_data_size != 0 || !finish)
                {
                    // C: `t->inDataSize_Start` / `t->inDataSize`, resolved into
                    // the slices `MtDec_Read` would have handed back.
                    let mut rem = in_data_size;
                    let mut link = 0usize;
                    let mut lim = in_data_size_start;
                    while rem != 0 {
                        let lim_now = if lim != 0 {
                            lim
                        } else {
                            self.in_buf_size.min(rem as usize)
                        };
                        lim = 0;
                        rem -= lim_now as u64;
                        let buf = std::mem::replace(&mut bufs[link], Vec::new().into_boxed_slice());
                        wr.replay.push_back(ReplayBuf { buf, len: lim_now });
                        link += 1;
                    }
                    bufs.retain(|b| !b.is_empty());
                    wr.num_filled_threads += 1;
                }
            }

            if !finish {
                let n = next_thread.expect("a next thread exists unless finishing");
                self.slots[n].can_write.set();
            } else if need_continue {
                // We restore decoding with a new iteration.
                self.slots[0].can_write.set();
                self.slots[0].can_read.set();
            } else {
                // We exit from decoding.
                if index == 0 {
                    return Ok(());
                }
                self.exit_thread.store(true, Ordering::Release);
                self.slots[0].can_read.set();
            }
        }
    }

    /// C: `MtDecThread_CreateAndStart`.
    fn spawn<'s>(&'s self, index: usize, scope: &'s Scope<'s, '_>) -> io::Result<()>
    where
        C: 's,
        'e: 's,
    {
        std::thread::Builder::new()
            .name(format!("lzma2-mt-{index}"))
            .spawn_scoped(scope, move || {
                let mut coder = None;
                let mut bufs: Vec<Box<[u8]>> = Vec::new();
                if let Err(e) = self.thread_func(index, scope, &mut coder, &mut bufs) {
                    match e {
                        MtError::Lzma(e) => self.set_error(e),
                        MtError::Io(e) => {
                            let mut wr = self.lock_write();
                            if wr.write_err.is_none() {
                                wr.write_err = Some(e);
                            }
                        }
                    }
                    self.exit_thread.store(true, Ordering::Release);
                    self.slots[0].can_read.set();
                    self.slots[0].can_write.set();
                }
            })
            .map(|_| ())
    }
}
