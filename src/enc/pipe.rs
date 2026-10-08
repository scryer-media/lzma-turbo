//! A push front end for the pull-driven LZMA2 encoder.
//!
//! [`Lzma2Encoder`] is the SDK's encoder, and the SDK's encoder pulls: its
//! match finder reads the window from a [`SeqInStream`] whenever it runs low,
//! and an empty read is the end of the input. A `Write` adapter is handed its
//! input instead, one call at a time, and must return between calls. Inverting
//! the encoder's loop would touch every layer of the port, so this bridges the
//! two with a thread: the encoder runs on it, reading from a bounded channel
//! that [`Lzma2Pipe::write`] feeds, and its output comes back over a second
//! channel that every call drains.
//!
//! What it holds is bounded whatever the input's length: [`INPUT_DEPTH`] full
//! chunks in the channel, the one the encoder is reading, the one being filled,
//! and the output not yet drained, on top of the encoder's own dictionary and
//! tables. The bytes are exactly what the same encoder writes over the same
//! input from a [`super::SliceStream`] with no data size set: the stream is
//! the encoder's only input path either way.
//!
//! [`Lzma2Pipe::start`] hands the encoder back when no thread can be started
//! (`wasm32-unknown-unknown` has none), so the caller can fall back to holding
//! the input and encoding it at the end.

use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::error::Error;

use super::lzma2_enc::Lzma2Encoder;
use super::stream::{SeqInStream, SeqOutStream};

/// Bytes per message on the input channel. A caller's large write is cut into
/// these, so that what is in flight does not depend on how much it hands over.
const CHUNK: usize = 256 << 10;

/// Full chunks the input channel holds before a write blocks.
const INPUT_DEPTH: usize = 2;

/// The encoder's output is batched to about this much per message, so a run
/// of small range-coder flushes is not one message each.
const OUT_BATCH: usize = 64 << 10;

/// The encoder's view of the input channel.
///
/// An empty chunk is the end of the input; a channel closed without one is a
/// writer dropped mid-stream, and the encoder is stopped with an error rather
/// than left to finish a stream nobody will read.
struct Source {
    rx: Receiver<Vec<u8>>,
    /// Spent chunks go back to the writer to be refilled.
    spent: Sender<Vec<u8>>,
    cur: Vec<u8>,
    pos: usize,
    ended: bool,
}

impl SeqInStream for Source {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if buf.is_empty() || self.ended {
            return Ok(0);
        }
        loop {
            let left = &self.cur[self.pos..];
            if !left.is_empty() {
                let n = buf.len().min(left.len());
                buf[..n].copy_from_slice(&left[..n]);
                self.pos += n;
                return Ok(n);
            }
            let done = core::mem::take(&mut self.cur);
            if done.capacity() == CHUNK {
                // The writer may already be gone; the chunk is then just freed.
                let _ = self.spent.send(done);
            }
            self.pos = 0;
            match self.rx.recv() {
                Ok(chunk) if chunk.is_empty() => {
                    self.ended = true;
                    return Ok(0);
                }
                Ok(chunk) => self.cur = chunk,
                Err(_) => return Err(Error::Read),
            }
        }
    }
}

/// The encoder's view of the output channel.
struct Sink {
    tx: Sender<Vec<u8>>,
    buf: Vec<u8>,
}

impl Sink {
    fn flush(&mut self) -> Result<(), Error> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let full = core::mem::take(&mut self.buf);
        // The receiver is gone only if the writer was dropped mid-stream.
        self.tx.send(full).map_err(|_| Error::Write)
    }
}

impl SeqOutStream for Sink {
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.buf.try_reserve(data.len()).map_err(|_| Error::Alloc)?;
        self.buf.extend_from_slice(data);
        if self.buf.len() >= OUT_BATCH {
            self.flush()?;
        }
        Ok(())
    }
}

/// One LZMA2 stream being encoded on its own thread from pushed input.
pub(crate) struct Lzma2Pipe {
    /// `None` once [`Lzma2Pipe::finish`] has sent the end of the input.
    input: Option<SyncSender<Vec<u8>>>,
    spent: Receiver<Vec<u8>>,
    output: Receiver<Vec<u8>>,
    worker: Option<JoinHandle<Result<(), Error>>>,
    /// The chunk being filled from the caller's writes.
    fill: Vec<u8>,
    /// Compressed bytes handed out so far.
    produced: u64,
}

impl Lzma2Pipe {
    /// Starts `enc` on a thread of its own, or hands it back if no thread
    /// could be started.
    ///
    /// The encoder runs with no data size: the input's length is not known
    /// until [`Lzma2Pipe::finish`]. Its block size and thread settings are
    /// left as the caller set them.
    pub(crate) fn start(enc: Lzma2Encoder) -> Result<Self, Box<Lzma2Encoder>> {
        let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(INPUT_DEPTH);
        let (spent_tx, spent_rx) = mpsc::channel::<Vec<u8>>();
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>();
        // The encoder goes over only once the thread exists, so that a failed
        // spawn - whose closure is dropped - does not take it down with it.
        let (enc_tx, enc_rx) = mpsc::sync_channel::<Lzma2Encoder>(1);
        let spawned = std::thread::Builder::new()
            .name("lzma-turbo lzma2 encoder".into())
            .spawn(move || {
                let mut enc = enc_rx.recv().map_err(|_| Error::InternalFailure)?;
                let mut source = Source {
                    rx: input_rx,
                    spent: spent_tx,
                    cur: Vec::new(),
                    pos: 0,
                    ended: false,
                };
                let mut sink = Sink {
                    tx: output_tx,
                    buf: Vec::new(),
                };
                enc.set_data_size(u64::MAX);
                enc.encode_mt(&mut source, &mut sink)?;
                sink.flush()
                // `sink` drops here, which is what ends `finish`'s drain.
            });
        let worker = match spawned {
            Ok(worker) => worker,
            Err(_) => return Err(Box::new(enc)),
        };
        if let Err(mpsc::SendError(enc)) = enc_tx.send(enc) {
            // Not reachable: the thread holds the receiver until it has read
            // from it. Give the encoder back rather than lose it.
            return Err(Box::new(enc));
        }
        Ok(Lzma2Pipe {
            input: Some(input_tx),
            spent: spent_rx,
            output: output_rx,
            worker: Some(worker),
            fill: Vec::new(),
            produced: 0,
        })
    }

    /// Feeds the next uncompressed bytes, appending to `out` whatever the
    /// encoder has produced meanwhile.
    ///
    /// # Errors
    ///
    /// The encoder's error if it has stopped, or [`Error::Alloc`].
    pub(crate) fn write(&mut self, mut data: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        while !data.is_empty() {
            if self.fill.capacity() == 0 {
                self.fill = match self.spent.try_recv() {
                    Ok(mut chunk) => {
                        chunk.clear();
                        chunk
                    }
                    Err(_) => {
                        let mut chunk = Vec::new();
                        chunk.try_reserve_exact(CHUNK).map_err(|_| Error::Alloc)?;
                        chunk
                    }
                };
            }
            let take = (CHUNK - self.fill.len()).min(data.len());
            self.fill.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.fill.len() == CHUNK {
                let full = core::mem::take(&mut self.fill);
                self.send(full, out)?;
            }
        }
        self.drain(out)
    }

    /// Hands a buffer the caller already owns to the encoder as it stands,
    /// after whatever earlier writes are still being collected.
    ///
    /// # Errors
    ///
    /// As [`Lzma2Pipe::write`].
    pub(crate) fn write_owned(&mut self, data: Vec<u8>, out: &mut Vec<u8>) -> Result<(), Error> {
        self.send_fill(out)?;
        if !data.is_empty() {
            self.send(data, out)?;
        }
        Ok(())
    }

    /// Ends the input, waits for the encoder, and appends the rest of the
    /// stream to `out`. Returns how many compressed bytes the whole stream
    /// came to.
    ///
    /// # Errors
    ///
    /// Whatever the encoder returned, or [`Error::InternalFailure`] if its
    /// thread panicked.
    pub(crate) fn finish(mut self, out: &mut Vec<u8>) -> Result<u64, Error> {
        self.send_fill(out)?;
        // An empty chunk is the end of the input.
        self.send(Vec::new(), out)?;
        drop(self.input.take());
        while let Ok(chunk) = self.output.recv() {
            self.append(&chunk, out)?;
        }
        match self.worker.take().map(JoinHandle::join) {
            Some(Ok(Ok(()))) => Ok(self.produced),
            Some(Ok(Err(e))) => Err(e),
            Some(Err(_)) | None => Err(Error::InternalFailure),
        }
    }

    /// Sends the chunk being filled, if there is anything in it.
    fn send_fill(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.fill.is_empty() {
            return Ok(());
        }
        let part = core::mem::take(&mut self.fill);
        self.send(part, out)
    }

    fn send(&mut self, chunk: Vec<u8>, out: &mut Vec<u8>) -> Result<(), Error> {
        let sent = match &self.input {
            Some(input) => input.send(chunk).is_ok(),
            None => false,
        };
        if !sent {
            return Err(self.stopped());
        }
        self.drain(out)
    }

    /// Appends whatever output is ready without waiting for more.
    fn drain(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        while let Ok(chunk) = self.output.try_recv() {
            self.append(&chunk, out)?;
        }
        Ok(())
    }

    fn append(&mut self, chunk: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        out.try_reserve(chunk.len()).map_err(|_| Error::Alloc)?;
        out.extend_from_slice(chunk);
        self.produced += chunk.len() as u64;
        Ok(())
    }

    /// The error of an encoder that stopped before its input ended.
    fn stopped(&mut self) -> Error {
        drop(self.input.take());
        match self.worker.take().map(JoinHandle::join) {
            Some(Ok(Err(e))) => e,
            // A clean finish with the input still open, or a panic.
            _ => Error::InternalFailure,
        }
    }
}
