#![cfg(all(feature = "std", feature = "crc", feature = "enc"))]

use lzma_turbo::{
    Checksum, ChecksumPlan, Error, Lzma2Run, Lzma2RunScanner, LzmaEncProps, encode_lzma2,
    run::Lzma2RunDecoder,
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

fn fixture(compressed: bool) -> (u8, Vec<u8>, Vec<u8>, Lzma2Run) {
    let original: Vec<_> = (0..600_019).map(|i| ((i * 29) ^ (i >> 7)) as u8).collect();
    let (prop, mut packed) = if compressed {
        encode_lzma2(
            &original,
            &LzmaEncProps::new().with_level(1).with_dict_size(1 << 16),
        )
        .unwrap()
    } else {
        let mut bytes = Vec::new();
        for (index, chunk) in original.chunks(65536).enumerate() {
            bytes.push(if index == 0 { 1 } else { 2 });
            bytes.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
            bytes.extend_from_slice(chunk);
        }
        bytes.push(0);
        (8, bytes)
    };
    let mut scanner = Lzma2RunScanner::new();
    scanner.feed(&packed).unwrap();
    let run = scanner.next_run().unwrap();
    assert!(scanner.next_run().is_none());
    assert_eq!(run.packed_len + 1, packed.len() as u64);
    assert_eq!(packed.pop(), Some(0));
    (prop, packed, original, run)
}

#[test]
fn ring_runs_preserve_bytes_split_crc_and_partial_write_offsets() {
    for compressed in [false, true] {
        let (prop, packed, original, mut run) = fixture(compressed);
        run.out_offset = 71;
        for input_step in [1, 4093, packed.len()] {
            let cancel = AtomicBool::new(false);
            let splits = [71 + 17, 71 + 65539, 71 + 280001];
            let plan = ChecksumPlan::new(Checksum::Crc32).with_split_points(splits);
            let memory = Lzma2RunDecoder::memory_required(prop, run.unpacked_len).unwrap()
                + Lzma2RunDecoder::checksum_memory_required(
                    &plan,
                    run.out_offset,
                    run.unpacked_len,
                )
                .unwrap();
            assert!(memory < run.unpacked_len);
            let mut decoder = Lzma2RunDecoder::new(3, prop, run, memory).unwrap();
            decoder.set_checksum_plan(&plan).unwrap();
            let mut actual = Vec::new();
            let mut interrupted = false;
            let mut sink = |offset, bytes: &[u8]| {
                interrupted = !interrupted;
                if interrupted {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                assert_eq!(offset, 71 + actual.len() as u64);
                let size = bytes.len().min(4093);
                actual.extend_from_slice(&bytes[..size]);
                Ok(size)
            };
            let mut pos = 0;
            while pos < packed.len() {
                let end = (pos + input_step).min(packed.len());
                let progress = decoder
                    .decode(&packed[pos..end], 37019, &cancel, &mut sink)
                    .unwrap();
                assert!(progress.read != 0 || progress.written != 0);
                assert!(progress.written <= 37019);
                pos += progress.read;
            }
            loop {
                let progress = decoder.decode(&[], 37019, &cancel, &mut sink).unwrap();
                if progress.written == 0 {
                    break;
                }
            }
            let completed = decoder.finish(&cancel).unwrap();
            assert_eq!(actual, original);
            assert_eq!(completed.index, 3);
            assert_eq!(completed.checks.len, original.len() as u64);
            assert_eq!(completed.checks.segments.len(), 4);
            for segment in completed.checks.segments {
                let start = (segment.offset - 71) as usize;
                let end = start + segment.len as usize;
                assert_eq!(
                    segment.check.crc32().unwrap(),
                    lzma_turbo::crc::crc32(&original[start..end])
                );
            }
        }
    }
}

#[test]
fn ring_runs_refuse_extra_early_eos_truncation_and_false_output_bounds() {
    let (prop, packed, _, run) = fixture(false);
    let cancel = AtomicBool::new(false);
    for case in 0..5 {
        let mut descriptor = run;
        let mut bytes = packed.clone();
        match case {
            0 => bytes.push(0),
            1 => bytes[0] = 0,
            2 => {
                bytes.pop();
            }
            3 => descriptor.unpacked_len -= 1,
            4 => descriptor.unpacked_len += 1,
            _ => unreachable!(),
        }
        let mut decoder = Lzma2RunDecoder::new(9, prop, descriptor, u64::MAX).unwrap();
        let decoded = decoder.decode(&bytes, usize::MAX, &cancel, &mut |_, data: &[u8]| {
            Ok(data.len())
        });
        let completion = decoder.finish(&cancel);
        assert!(decoded.is_err() || completion.is_err(), "case {case}");
        assert!(matches!(
            completion,
            Err(Error::CorruptRun {
                index: 9,
                out_offset: 0
            })
        ));
    }
}

#[test]
fn failed_or_cancelled_ring_writes_never_complete() {
    let (prop, packed, _, run) = fixture(false);
    for cancel_write in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut decoder = Lzma2RunDecoder::new(5, prop, run, u64::MAX).unwrap();
        let mut accepted = 0;
        let error = decoder
            .decode(&packed, usize::MAX, &cancel, &mut |_, bytes: &[u8]| {
                if accepted == 0 {
                    accepted = bytes.len().min(17);
                    if cancel_write {
                        cancel.store(true, Ordering::Release);
                    }
                    Ok(accepted)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "sink refused",
                    ))
                }
            })
            .unwrap_err();
        assert_eq!(accepted, 17);
        assert_eq!(
            error.kind(),
            if cancel_write {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::PermissionDenied
            }
        );
        cancel.store(false, Ordering::Release);
        assert!(
            decoder
                .decode(&[], 1, &cancel, &mut |_, _| panic!("poisoned sink"))
                .is_err()
        );
        assert!(matches!(
            decoder.finish(&cancel),
            Err(Error::CorruptRun { index: 5, .. })
        ));
    }
}

#[test]
fn oversized_dictionary_is_capped_and_memory_checked_before_allocation() {
    let (_, packed, original, run) = fixture(false);
    let required = Lzma2RunDecoder::memory_required(40, run.unpacked_len).unwrap();
    assert_eq!(
        required,
        (original.len() as u64).div_ceil(4096) * 4096 + 65536
    );
    assert!(matches!(
        Lzma2RunDecoder::new(0, 40, run, required - 1),
        Err(Error::Alloc)
    ));
    let mut decoder = Lzma2RunDecoder::new(0, 40, run, required).unwrap();
    let cancel = AtomicBool::new(false);
    let mut actual = Vec::new();
    decoder
        .decode(&packed, usize::MAX, &cancel, &mut |_, bytes: &[u8]| {
            actual.extend_from_slice(bytes);
            Ok(bytes.len())
        })
        .unwrap();
    decoder.finish(&cancel).unwrap();
    assert_eq!(actual, original);
}

#[test]
fn stored_payload_is_delivered_from_input_without_a_ring_readback() {
    let (prop, packed, original, run) = fixture(false);
    let cancel = AtomicBool::new(false);
    let mut decoder = Lzma2RunDecoder::new(0, prop, run, u64::MAX).unwrap();
    let mut actual = Vec::new();
    decoder
        .decode(&packed, usize::MAX, &cancel, &mut |_, bytes: &[u8]| {
            let start = packed.as_ptr() as usize;
            let pointer = bytes.as_ptr() as usize;
            assert!(pointer >= start && pointer + bytes.len() <= start + packed.len());
            actual.extend_from_slice(bytes);
            Ok(bytes.len())
        })
        .unwrap();
    decoder.finish(&cancel).unwrap();
    assert_eq!(actual, original);
}

#[test]
fn checksum_segments_are_reserved_before_a_run_can_write() {
    let (prop, packed, _, run) = fixture(false);
    let plan = ChecksumPlan::new(Checksum::Crc32).with_split_points(1..1000);
    let memory = Lzma2RunDecoder::memory_required(prop, run.unpacked_len).unwrap();
    let checks = Lzma2RunDecoder::checksum_memory_required(&plan, 0, run.unpacked_len).unwrap();
    assert!(checks >= 1000 * 24);
    let mut decoder = Lzma2RunDecoder::new(0, prop, run, memory + checks - 1).unwrap();
    assert_eq!(decoder.set_checksum_plan(&plan), Err(Error::Alloc));
    assert!(
        decoder
            .decode(
                &packed,
                usize::MAX,
                &AtomicBool::new(false),
                &mut |_, _| panic!("unreserved checksum storage")
            )
            .is_err()
    );
    let mut decoder = Lzma2RunDecoder::new(0, prop, run, memory + checks).unwrap();
    decoder.set_checksum_plan(&plan).unwrap();
    decoder
        .decode(
            &packed,
            usize::MAX,
            &AtomicBool::new(false),
            &mut |_, bytes: &[u8]| Ok(bytes.len()),
        )
        .unwrap();
    assert_eq!(
        decoder
            .finish(&AtomicBool::new(false))
            .unwrap()
            .checks
            .segments
            .len(),
        1000
    );
}
