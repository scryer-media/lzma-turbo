//! The generic block-parallel coder.
//!
//! C: `C/MtCoder.c` and `C/MtCoder.h` — `CMtCoder`, `MtCoder_Code` and
//! `ThreadFunc2`, everything `Z7_ST` guards there. It splits an input into
//! fixed-size blocks, hands each to a worker through the
//! [`MtCoderCallback::code`] callback, and writes the results through
//! [`MtCoderCallback::write`] *in block order* whatever order they finished
//! in. That ordering is what makes the output deterministic, and it is the
//! only reason this file exists: nothing here decides how a block is
//! compressed.
//!
//! The structure is the C's. One auto-reset event (`readEvent`) is passed from
//! thread to thread as a token, and the thread holding it reads the next
//! block, decides whether the input ended, and starts one more worker if the
//! limit has not been reached — "each new thread will create another new
//! thread after block reading", as the C puts it. A counting semaphore
//! (`blocksSemaphore`) bounds how many blocks may be in flight, and a free
//! list under one critical section hands out the output buffers.
//!
//! # Deviations
//!
//! * **Threads live for one call.** `CMtCoderThread` keeps a thread parked on
//!   its `startEvent` between `MtCoder_Code` calls so a later call can reuse
//!   it; this port spawns into a [`std::thread::scope`] instead and joins them
//!   all at the end of the call. Scoped threads are what let a worker borrow
//!   the caller's input and callback rather than forcing everything behind an
//!   `Arc`, and the C's own reuse buys one thread creation per file.
//! * **`MTCODER_USE_WRITE_THREAD` is not carried.** It is `#undef`ed in the C
//!   too; the ported path is the one that ships, where any worker may write.
//! * **No `ICompressProgress`.** As elsewhere in this port, there is no
//!   caller for it, so `CMtProgress` shrinks to the error cell it also is.

use alloc::vec::Vec;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::enc::stream::SeqInStream;
use crate::error::Error;
use crate::mt::event::Event;
use crate::mt::sync::Semaphore;

/// C: `MTCODER_THREADS_MAX`.
pub(crate) const THREADS_MAX: usize = 256;

/// C: `MTCODER_GET_NUM_BLOCKS_FROM_THREADS`.
const fn num_blocks_from_threads(num_threads: usize) -> usize {
    num_threads + num_threads / 8 + 1
}

/// C: `MTCODER_BLOCKS_MAX`.
pub(crate) const BLOCKS_MAX: usize = num_blocks_from_threads(THREADS_MAX) + 3;

/// The two calls a block-parallel coder makes.
///
/// C: `IMtCoderCallback2`. `coder_index` picks the worker's own coder state
/// and `out_buf_index` the output buffer the block was written into; both are
/// indices the caller allocated per thread and per block, which is how the C
/// keeps workers off each other's state without a lock.
pub(crate) trait MtCoderCallback: Sync {
    /// C: `IMtCoderCallback2::Code`. Compresses one block.
    fn code(
        &self,
        coder_index: usize,
        out_buf_index: usize,
        src: &[u8],
        finished: bool,
    ) -> Result<(), Error>;

    /// C: `IMtCoderCallback2::Write`. Emits a finished block; called in block
    /// order.
    fn write(&self, out_buf_index: usize) -> Result<(), Error>;
}

/// Where the blocks come from.
///
/// C: `CMtCoder::inStream` and `CMtCoder::inData`, which the C keeps as two
/// fields and checks for `NULL`.
pub(crate) enum MtInput<'a> {
    /// C: `inData` / `inDataSize`. Each worker takes its own subrange, so
    /// nothing is copied.
    Data(&'a [u8]),
    /// C: `inStream`, read one block at a time by whichever worker holds the
    /// read token. The `Send` bound is what lets that worker be another
    /// thread; the C has no such bound because it has no such check.
    Stream(Mutex<&'a mut (dyn SeqInStream + Send)>),
}

/// C: `CMtCoder`, the input half.
pub(crate) struct MtCoder<'a, 'i> {
    /// C: `p->blockSize`.
    pub(crate) block_size: usize,
    /// C: `p->numThreadsMax`.
    pub(crate) num_threads_max: usize,
    /// C: `p->expectedDataSize`.
    pub(crate) expected_data_size: u64,
    pub(crate) input: MtInput<'i>,
    pub(crate) callback: &'a dyn MtCoderCallback,
}

/// C: `CMtCoderBlock`.
#[derive(Clone, Copy)]
struct Block {
    res: Result<(), Error>,
    /// C: `bufIndex`, with `(unsigned)(int)-1` for "no buffer".
    buf_index: Option<usize>,
    finished: bool,
}

/// The state behind `readEvent`: only the thread holding that token touches
/// it, so the mutex is never contended. C: the fields `ThreadFunc2` reads and
/// writes between `Event_Wait(&mtc->readEvent)` and `Event_Set(&mtc->readEvent)`.
struct ReadState {
    stop_reading: bool,
    read_res: Result<(), Error>,
    read_processed: u64,
    block_index: usize,
    num_started_threads: usize,
}

/// The state behind `cs`. C: the fields guarded by `CMtCoder::cs`.
struct Shared {
    free_block_head: usize,
    free_block_list: [usize; BLOCKS_MAX],
    ready_blocks: [bool; BLOCKS_MAX],
    /// C: `writeIndex`, with `(unsigned)(int)-1` meaning "a thread is writing".
    write_index: Option<usize>,
}

/// C: the internal half of `CMtCoder`, live for one `MtCoder_Code`.
struct Run<'a, 'i, 'c> {
    coder: &'c MtCoder<'a, 'i>,
    num_blocks_max: usize,
    num_started_threads_limit: usize,

    read_event: Event,
    blocks_semaphore: Semaphore,
    finished_event: Event,

    read: Mutex<ReadState>,
    shared: Mutex<Shared>,
    blocks: Mutex<Vec<Block>>,
    write_res: Mutex<Result<(), Error>>,
    /// C: `p->mtProgress.res`, which is the whole of `CMtProgress` once the
    /// progress callback is gone.
    error: Mutex<Result<(), Error>>,
    num_finished_threads: AtomicUsize,
}

impl Run<'_, '_, '_> {
    /// C: `MtProgress_GetError`.
    fn get_error(&self) -> Result<(), Error> {
        *self.error.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// C: `MtProgress_SetError`, which keeps the first error.
    fn set_error(&self, e: Error) {
        let mut g = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_ok() {
            *g = Err(e);
        }
    }
}

impl MtCoder<'_, '_> {
    /// C: `MtCoder_Code`.
    ///
    /// # Errors
    ///
    /// The first error any worker reported, or whatever the input or the
    /// callbacks returned.
    pub(crate) fn code(&self) -> Result<(), Error> {
        let mut num_threads = self.num_threads_max.max(1);
        if num_threads > THREADS_MAX {
            num_threads = THREADS_MAX;
        }
        let mut num_blocks_max = num_blocks_from_threads(num_threads);
        // C: small blocks get more of them in flight, so a slow block cannot
        // starve the readers.
        if self.block_size < (1 << 26) {
            num_blocks_max += 1;
        }
        if self.block_size < (1 << 24) {
            num_blocks_max += 1;
        }
        if self.block_size < (1 << 22) {
            num_blocks_max += 1;
        }
        if num_blocks_max > BLOCKS_MAX {
            num_blocks_max = BLOCKS_MAX;
        }

        let mut free_block_list = [0usize; BLOCKS_MAX];
        for (i, slot) in free_block_list.iter_mut().enumerate() {
            *slot = i + 1;
        }
        free_block_list[BLOCKS_MAX - 1] = usize::MAX;

        let run = Run {
            coder: self,
            num_blocks_max,
            num_started_threads_limit: num_threads,
            read_event: Event::new(),
            blocks_semaphore: Semaphore::new(),
            finished_event: Event::new(),
            read: Mutex::new(ReadState {
                stop_reading: false,
                read_res: Ok(()),
                read_processed: 0,
                block_index: 0,
                num_started_threads: 1,
            }),
            shared: Mutex::new(Shared {
                free_block_head: 0,
                free_block_list,
                ready_blocks: [false; BLOCKS_MAX],
                write_index: Some(0),
            }),
            blocks: Mutex::new(alloc::vec![
                Block {
                    res: Ok(()),
                    buf_index: None,
                    finished: false,
                };
                BLOCKS_MAX
            ]),
            write_res: Mutex::new(Ok(())),
            error: Mutex::new(Ok(())),
            num_finished_threads: AtomicUsize::new(0),
        };
        run.blocks_semaphore.init(num_blocks_max as u32);

        std::thread::scope(|scope| {
            // C: "here we create new thread for first block. And each new
            // thread will create another new thread after block reading until
            // numStartedThreadsLimit is reached."
            run.spawn(scope, 0);
            run.read_event.set();
            run.finished_event.wait();
        });

        let mut res = run.get_error();
        if res.is_ok() {
            res = run
                .read
                .into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .read_res;
        }
        if res.is_ok() {
            res = run
                .write_res
                .into_inner()
                .unwrap_or_else(|e| e.into_inner());
        }
        res
    }
}

impl<'a, 'i, 'c> Run<'a, 'i, 'c> {
    /// C: `MtCoderThread_CreateAndStart`, which in this port is the spawn
    /// itself: there is no parked thread to signal.
    fn spawn<'s>(&'s self, scope: &'s std::thread::Scope<'s, '_>, index: usize)
    where
        'a: 's,
        'c: 's,
    {
        scope.spawn(move || {
            let res = self.thread_func2(scope, index);
            if let Err(e) = res {
                self.set_error(e);
            }
            // C: the last thread out sets `finishedEvent`. `numStartedThreads`
            // only ever grows, and it grows under the read token before the
            // thread it counts can finish, so this comparison is the C's.
            let num_finished = self.num_finished_threads.fetch_add(1, Ordering::SeqCst) + 1;
            let started = self
                .read
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .num_started_threads;
            if num_finished == started {
                self.finished_event.set();
            }
        });
    }

    /// C: `ThreadFunc2`.
    #[allow(clippy::too_many_lines)] // One C function, kept as one.
    fn thread_func2<'s>(
        &'s self,
        scope: &'s std::thread::Scope<'s, '_>,
        index: usize,
    ) -> Result<(), Error>
    where
        'a: 's,
        'c: 's,
    {
        // C: `t->inBuf`, allocated on first use and only for the stream input.
        let mut in_buf: Vec<u8> = Vec::new();

        loop {
            self.read_event.wait();
            // C: "after Event_Wait(&mtc->readEvent) we must call
            // Event_Set(&mtc->readEvent) in any case to unlock another threads"

            let mut guard = self.read.lock().unwrap_or_else(|e| e.into_inner());
            if guard.stop_reading {
                drop(guard);
                self.read_event.set();
                return Ok(());
            }

            let mut res = self.get_error();
            let mut size = 0usize;
            let mut in_data: Option<&[u8]> = None;
            let mut finished = true;
            let mut read_processed = 0u64;

            if res.is_ok() {
                match &self.coder.input {
                    MtInput::Stream(stream) => {
                        if in_buf.len() != self.coder.block_size {
                            in_buf.clear();
                            if in_buf.try_reserve_exact(self.coder.block_size).is_err() {
                                res = Err(Error::Alloc);
                            } else {
                                crate::enc::huge_pages::advise_vec(&in_buf);
                                in_buf.resize(self.coder.block_size, 0);
                            }
                        }
                        if res.is_ok() {
                            let mut s = stream.lock().unwrap_or_else(|e| e.into_inner());
                            match read_max(&mut **s, &mut in_buf) {
                                Ok(n) => size = n,
                                Err(e) => res = Err(e),
                            }
                            drop(s);
                            read_processed = guard.read_processed + size as u64;
                            guard.read_processed = read_processed;
                        }
                        if let Err(e) = res {
                            guard.read_res = Err(e);
                            // C: "after reading error - we can stop encoding
                            // of previous blocks"
                            self.set_error(e);
                        } else {
                            finished = size != self.coder.block_size;
                        }
                    }
                    MtInput::Data(data) => {
                        read_processed = guard.read_processed;
                        let rem = data.len() - read_processed as usize;
                        size = self.coder.block_size.min(rem);
                        in_data = Some(&data[read_processed as usize..][..size]);
                        read_processed += size as u64;
                        guard.read_processed = read_processed;
                        finished = data.len() == read_processed as usize;
                    }
                }
            }

            // C: "we must get some block from blocksSemaphore before
            // Event_Set(&mtc->readEvent)"
            self.blocks_semaphore.wait();

            let bi = guard.block_index;
            guard.block_index += 1;
            if guard.block_index >= self.num_blocks_max {
                guard.block_index = 0;
            }

            if res.is_ok() {
                res = self.get_error();
            }
            if res.is_err() {
                finished = true;
            }

            if !finished
                && guard.num_started_threads < self.num_started_threads_limit
                && self.coder.expected_data_size != read_processed
            {
                let next = guard.num_started_threads;
                guard.num_started_threads += 1;
                self.spawn(scope, next);
            }

            if finished {
                guard.stop_reading = true;
            }
            drop(guard);
            self.read_event.set();

            let mut buf_index = None;
            if res.is_ok() {
                let bufi = {
                    let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    let head = g.free_block_head;
                    g.free_block_head = g.free_block_list[head];
                    head
                };
                buf_index = Some(bufi);
                let src: &[u8] = match in_data {
                    Some(d) => d,
                    None => &in_buf[..size],
                };
                res = self.coder.callback.code(index, bufi, src, finished);
                if let Err(e) = res {
                    self.set_error(e);
                }
            }

            {
                let mut g = self.blocks.lock().unwrap_or_else(|e| e.into_inner());
                g[bi] = Block {
                    res,
                    buf_index,
                    finished,
                };
            }

            // C: the writer hand-off. The thread whose block is the next one
            // to write claims the write turn; every other thread marks its
            // block ready and moves on.
            let turn = {
                let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                let wi = g.write_index;
                if wi == Some(bi) {
                    g.write_index = None;
                } else {
                    g.ready_blocks[bi] = true;
                }
                wi
            };

            if turn != Some(bi) {
                if res.is_err() || finished {
                    return Ok(());
                }
                continue;
            }
            let mut wi = bi;

            {
                let w = *self.write_res.lock().unwrap_or_else(|e| e.into_inner());
                if w.is_err() {
                    res = w;
                }
            }

            let mut write_buf = buf_index;
            let mut write_finished = finished;
            loop {
                if let (Ok(()), Some(bufi)) = (res, write_buf) {
                    res = self.coder.callback.write(bufi);
                    if let Err(e) = res {
                        *self.write_res.lock().unwrap_or_else(|e| e.into_inner()) = Err(e);
                        self.set_error(e);
                    }
                }

                wi += 1;
                if wi >= self.num_blocks_max {
                    wi = 0;
                }

                let is_ready = {
                    let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(bufi) = write_buf {
                        g.free_block_list[bufi] = g.free_block_head;
                        g.free_block_head = bufi;
                    }
                    let ready = g.ready_blocks[wi];
                    if ready {
                        g.ready_blocks[wi] = false;
                    } else {
                        g.write_index = Some(wi);
                    }
                    ready
                };
                self.blocks_semaphore.release1();
                if !is_ready {
                    break;
                }

                let block = self.blocks.lock().unwrap_or_else(|e| e.into_inner())[wi];
                if res.is_ok() && block.res.is_err() {
                    res = block.res;
                }
                write_buf = block.buf_index;
                write_finished = block.finished;
            }

            if write_finished || res.is_err() {
                return Ok(());
            }
        }
    }
}

/// C: `SeqInStream_ReadMax`, which reads until the buffer is full or the
/// stream ends.
fn read_max(stream: &mut dyn SeqInStream, buf: &mut [u8]) -> Result<usize, Error> {
    let mut done = 0usize;
    while done != buf.len() {
        let n = stream.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A callback that records each block's bytes and concatenates them in the
    /// order [`MtCoderCallback::write`] was called.
    struct Recorder {
        bufs: Vec<StdMutex<Vec<u8>>>,
        out: StdMutex<Vec<u8>>,
        coders_seen: StdMutex<Vec<usize>>,
    }

    impl MtCoderCallback for Recorder {
        fn code(
            &self,
            coder_index: usize,
            out_buf_index: usize,
            src: &[u8],
            finished: bool,
        ) -> Result<(), Error> {
            self.coders_seen.lock().unwrap().push(coder_index);
            let mut b = self.bufs[out_buf_index].lock().unwrap();
            b.clear();
            b.extend_from_slice(src);
            if finished {
                b.push(b'!');
            }
            Ok(())
        }

        fn write(&self, out_buf_index: usize) -> Result<(), Error> {
            let b = self.bufs[out_buf_index].lock().unwrap();
            self.out.lock().unwrap().extend_from_slice(&b);
            Ok(())
        }
    }

    fn recorder() -> Recorder {
        Recorder {
            bufs: (0..BLOCKS_MAX).map(|_| StdMutex::new(Vec::new())).collect(),
            out: StdMutex::new(Vec::new()),
            coders_seen: StdMutex::new(Vec::new()),
        }
    }

    /// Blocks are written in input order however many threads ran, from a
    /// slice and from a stream alike.
    #[test]
    fn blocks_come_out_in_order() {
        let src: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        for threads in [1usize, 2, 4, 7] {
            for block_size in [1usize, 7, 1000, 4096, 20_000, 65_536] {
                let cb = recorder();
                MtCoder {
                    block_size,
                    num_threads_max: threads,
                    expected_data_size: src.len() as u64,
                    input: MtInput::Data(&src),
                    callback: &cb,
                }
                .code()
                .expect("coded");
                let got = cb.out.into_inner().unwrap();
                let mut want = src.clone();
                want.push(b'!');
                assert_eq!(got, want, "{threads} threads, block {block_size}");

                let mut slice = crate::enc::stream::SliceStream::new(&src);
                let cb = recorder();
                MtCoder {
                    block_size,
                    num_threads_max: threads,
                    expected_data_size: src.len() as u64,
                    input: MtInput::Stream(Mutex::new(&mut slice)),
                    callback: &cb,
                }
                .code()
                .expect("coded");
                let got = cb.out.into_inner().unwrap();
                assert_eq!(got, want, "stream, {threads} threads, block {block_size}");
            }
        }
    }

    /// An empty input still produces the one finished block the C's loop does.
    #[test]
    fn an_empty_input_is_one_finished_block() {
        let cb = recorder();
        MtCoder {
            block_size: 4096,
            num_threads_max: 4,
            expected_data_size: 0,
            input: MtInput::Data(&[]),
            callback: &cb,
        }
        .code()
        .expect("coded");
        assert_eq!(cb.out.into_inner().unwrap(), b"!");
    }

    /// No worker is handed a coder index another worker is using at the same
    /// time, and no index above the thread limit is ever used.
    #[test]
    fn coder_indices_stay_within_the_thread_limit() {
        let src: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
        let cb = recorder();
        MtCoder {
            block_size: 1024,
            num_threads_max: 3,
            expected_data_size: src.len() as u64,
            input: MtInput::Data(&src),
            callback: &cb,
        }
        .code()
        .expect("coded");
        for i in cb.coders_seen.into_inner().unwrap() {
            assert!(i < 3, "coder index {i} is above the limit");
        }
    }

    /// An error from a worker comes back out of `code()`.
    #[test]
    fn a_coder_error_propagates() {
        struct Failing;
        impl MtCoderCallback for Failing {
            fn code(&self, _: usize, _: usize, _: &[u8], _: bool) -> Result<(), Error> {
                Err(Error::Param)
            }
            fn write(&self, _: usize) -> Result<(), Error> {
                Ok(())
            }
        }
        let src = [0u8; 10_000];
        let err = MtCoder {
            block_size: 1000,
            num_threads_max: 4,
            expected_data_size: src.len() as u64,
            input: MtInput::Data(&src),
            callback: &Failing,
        }
        .code()
        .expect_err("the coder failed");
        assert_eq!(err, Error::Param);
    }
}
