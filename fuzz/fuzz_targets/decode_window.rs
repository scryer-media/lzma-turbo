//! Arbitrary bytes as a raw LZMA2 stream, decoded once on the decoder's own
//! dictionary and once on caller memory filled with garbage. The two must
//! agree byte for byte, and fail or finish together: a decoder that read a
//! dictionary byte it had not written would tell them apart.
//!
//! A stream the single-threaded decoder takes is then decoded by two threads
//! through the parallel decoder's `decode` and `decode_at` and the adaptive
//! decoder, once in RAM and once with every worker's window on garbage-filled
//! caller memory; each pair must fail or finish together, with the same
//! bytes.
//!
//! The same bytes are then encoded as a small-block `.xz` file and decoded
//! by two threads, each block on its worker's own garbage-filled window; the
//! output must be the input.
#![no_main]

use std::io::{self, Cursor, Read};
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use lzma_turbo::xz::{CheckType, XzOptions, XzParallelReader};
use lzma_turbo::{
    DrainStatus, Lzma2AdaptiveDecoder, Lzma2MtOptions, Lzma2ParallelDecoder, Lzma2Reader,
    LzmaEncProps, WindowAllocator, WindowBacking, WindowMemory, encode_xz,
};

/// The most output the threaded decodes are given; past it they fail.
const MAX_OUT: usize = 1 << 24;

struct Garbage(Box<[u8]>);

// SAFETY: a boxed slice is initialized, does not move when the box does,
// and is owned by this value alone until it drops.
unsafe impl WindowMemory for Garbage {
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

struct Heap(u8);

impl WindowAllocator for Heap {
    fn allocate(&self, len: usize) -> io::Result<Box<dyn WindowMemory>> {
        let fill = self.0;
        let buf = (0..len)
            .map(|i| (i as u8).wrapping_mul(151) ^ fill)
            .collect();
        Ok(Box::new(Garbage(buf)))
    }
}

/// Reads `r` in `step`-byte reads, stopping at an error or after 256 MiB.
fn drain(mut r: impl Read, step: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = vec![0u8; step];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return (out, true),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => return (out, false),
        }
        if out.len() > (1 << 28) {
            return (out, true);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let step = match data[0] % 4 {
        0 => 1usize,
        1 => 7,
        2 => 4096,
        _ => 1 << 16,
    };
    // Cap the dictionary so the fuzzer cannot drive a multi-gigabyte alloc.
    let dict_prop = data[1] % 27;
    let input = &data[2..];
    let Ok(owned) = Lzma2Reader::with_memory_limit(input, dict_prop, u64::MAX) else {
        return;
    };
    let window = WindowBacking::new(Arc::new(Heap(data[0])), 0);
    let caller = Lzma2Reader::with_window(input, dict_prop, u64::MAX, window)
        .expect("a heap window never fails");
    let (reference, ok) = drain(owned, step);
    assert_eq!((reference.clone(), ok), drain(caller, step));

    if ok && reference.len() <= MAX_OUT {
        threaded(dict_prop, input, data[0]);
    }

    // Kept small: the encode and the two threads are per input.
    if input.len() > 1 << 16 {
        return;
    }
    let props = LzmaEncProps::new().with_level(1).with_dict_size(4096);
    let block = 1024u64 << (data[1] % 4);
    let packed = encode_xz(input, &props, CheckType::Crc32, block).expect("encode");
    let window = WindowBacking::new(Arc::new(Heap(data[1])), 0);
    let opts = XzOptions::default().with_threads(2).with_window(window);
    let reader = XzParallelReader::with_options(Cursor::new(&packed), opts).expect("our own file");
    assert_eq!(drain(reader, step), (input.to_vec(), true));
});

/// Each threaded decode at two threads, in RAM and on caller memory: the
/// pair must agree on success and, when they finish, on every byte.
fn threaded(dict_prop: u8, input: &[u8], fill: u8) {
    let opts = Lzma2MtOptions {
        threads: 2,
        memory_limit: 1 << 26,
    };
    let lent = || WindowBacking::new(Arc::new(Heap(fill)), 0);
    let (Ok(owned), Ok(caller)) = (
        Lzma2ParallelDecoder::new(dict_prop, &opts),
        Lzma2ParallelDecoder::with_window(dict_prop, &opts, lent()),
    ) else {
        return;
    };
    let decode = |d: &Lzma2ParallelDecoder| {
        let mut out = Vec::new();
        let ok = d.decode(input, Capped(&mut out)).is_ok();
        (ok, ok.then_some(out))
    };
    assert_eq!(decode(&owned), decode(&caller), "decode");
    let decode_at = |d: &Lzma2ParallelDecoder| {
        let mut out = Vec::new();
        let ok = d
            .decode_at(input, 0, input.len() as u64, Capped(&mut out))
            .is_ok();
        (ok, ok.then_some(out))
    };
    assert_eq!(decode_at(&owned), decode_at(&caller), "decode_at");
    let (Ok(owned), Ok(caller)) = (
        Lzma2AdaptiveDecoder::new(dict_prop, &opts),
        Lzma2AdaptiveDecoder::with_window(dict_prop, &opts, lent()),
    ) else {
        return;
    };
    assert_eq!(adaptive(owned, input), adaptive(caller, input), "adaptive");
}

/// Feeds the whole stream and drains it; `None` when the decode fails.
fn adaptive(mut d: Lzma2AdaptiveDecoder, mut input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if input.is_empty() {
        d.end_of_input();
    }
    loop {
        if !input.is_empty() {
            input = &input[d.feed(input).ok()?..];
            if input.is_empty() {
                d.end_of_input();
            }
        }
        let mut over = false;
        let status = d.drain(|_, b| {
            over |= out.len() + b.len() > MAX_OUT;
            if !over {
                out.extend_from_slice(b);
            }
        });
        match status.ok()? {
            _ if over => return None,
            DrainStatus::Finished => return Some(out),
            DrainStatus::NeedsMoreInput if input.is_empty() => return None,
            _ => {}
        }
    }
}

/// A sink that fails past [`MAX_OUT`].
struct Capped<'a>(&'a mut Vec<u8>);

impl io::Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.0.len() + buf.len() > MAX_OUT {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
