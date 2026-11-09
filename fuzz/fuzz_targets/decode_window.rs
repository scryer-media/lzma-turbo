//! Arbitrary bytes as a raw LZMA2 stream, decoded once on the decoder's own
//! dictionary and once on caller memory filled with garbage. The two must
//! agree byte for byte, and fail or finish together: a decoder that read a
//! dictionary byte it had not written would tell them apart.
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
    Lzma2Reader, LzmaEncProps, WindowAllocator, WindowBacking, WindowMemory, encode_xz,
};

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
    assert_eq!(drain(owned, step), drain(caller, step));

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
