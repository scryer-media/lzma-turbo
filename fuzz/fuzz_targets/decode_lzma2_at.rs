//! Arbitrary bytes as a raw LZMA2 stream sitting at an offset inside a larger
//! source, decoded by `Lzma2ParallelDecoder::decode_at`, by `decode` over the
//! same bytes, and by the single-threaded decoder.
//!
//! The property is agreement. A stream that ends at an end marker gives the
//! same bytes from all three; one that does not is refused by all three. A
//! refused stream may leave different amounts of output behind - `decode`
//! drops a whole block on an error inside it, `decode_at` keeps every run that
//! decoded - but whatever either wrote must be what the single-threaded
//! decoder writes over the same span. `decode_at` scans run boundaries itself
//! and hands anything it cannot split to `decode`, so a disagreement means the
//! scan, the in-order writer or the hand-over lost or duplicated output.
#![no_main]

use std::io::Write;

use libfuzzer_sys::fuzz_target;
use lzma_turbo::{FinishMode, Lzma2Decoder, Lzma2MtOptions, Lzma2ParallelDecoder, Status};

/// Bound on the output any path may produce before the case is abandoned.
const MAX_OUT: usize = 1 << 24;
/// Bound on what the parallel decoder may hold in flight.
const MEM_LIMIT: u64 = 1 << 26;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let threads = usize::from(data[0] % 8) + 1;
    // Cap the dictionary so the fuzzer cannot drive a multi-gigabyte alloc.
    let dict_prop = data[1] % 27;
    // Junk in front of the stream, which `decode_at` must never read.
    let lead = usize::from(data[2] % 17);
    let bounded = data[0] & 0x80 != 0;
    let stream = &data[3..];

    let Some((reference, ref_ok)) = single_threaded(dict_prop, stream) else {
        return;
    };
    let opts = Lzma2MtOptions {
        threads,
        memory_limit: MEM_LIMIT,
    };
    let Ok(dec) = Lzma2ParallelDecoder::new(dict_prop, &opts) else {
        return;
    };

    let mut by_stream = Capped::default();
    let stream_ok = dec.decode(stream, &mut by_stream).is_ok();

    let mut src = vec![0xA5u8; lead];
    src.extend_from_slice(stream);
    let len = if bounded {
        stream.len() as u64
    } else {
        u64::MAX
    };
    let mut by_offset = Capped::default();
    let res = dec.decode_at(src.as_slice(), lead as u64, len, &mut by_offset);
    if let Ok(n) = &res {
        assert_eq!(*n, by_offset.out.len() as u64, "decode_at miscounted");
    }

    for (who, ok, out) in [
        ("decode", stream_ok, &by_stream.out),
        ("decode_at", res.is_ok(), &by_offset.out),
    ] {
        assert_eq!(ok, ref_ok, "{who} disagrees on accepting the stream");
        if ref_ok {
            assert_eq!(out, &reference, "{who} decoded different bytes");
        } else {
            let n = out.len().min(reference.len());
            assert_eq!(
                out[..n],
                reference[..n],
                "{who} wrote bytes the single-threaded decoder does not"
            );
        }
    }
});

/// The reference: the bytes the single-threaded decoder produced and whether
/// the stream ended at an end marker; `None` when it outgrew [`MAX_OUT`]. A
/// failing call's own output is not kept, so on a refused stream these bytes
/// can stop short of the error.
fn single_threaded(dict_prop: u8, mut input: &[u8]) -> Option<(Vec<u8>, bool)> {
    let mut dec = Lzma2Decoder::new(dict_prop).ok()?;
    let mut out = vec![0u8; 4096];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        let Ok(p) = dec.decode(input, &mut out, FinishMode::Any) else {
            return Some((acc, false));
        };
        input = &input[p.read..];
        acc.extend_from_slice(&out[..p.written]);
        if acc.len() > MAX_OUT {
            return None;
        }
        if p.status == Status::FinishedWithMark {
            return Some((acc, true));
        }
        if p.read == 0 && p.written == 0 {
            // Out of input without an end marker: truncated.
            return Some((acc, false));
        }
    }
}

/// A sink that refuses to grow past [`MAX_OUT`]; the reference has already
/// established that an accepted stream fits.
#[derive(Default)]
struct Capped {
    out: Vec<u8>,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.out.len() + buf.len() > MAX_OUT {
            return Err(std::io::Error::other("output cap"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
