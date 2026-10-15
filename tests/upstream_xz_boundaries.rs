//! Format-level cases adapted from XZ Utils 5.8.3 tests (0BSD).
//! See docs/upstream-test-coverage.md for the complete applicability inventory.
#![cfg(feature = "xz")]

use lzma_turbo::crc::crc32;
use lzma_turbo::xz::{CheckType, StreamFlags, StreamFooter, StreamHeader, XzErrorKind};
use lzma_turbo::xz::{index::XzIndex, vli};

#[test]
fn upstream_vli_vectors_and_every_truncation() {
    // test_vli.c: independent, pre-encoded one-through-nine-byte vectors.
    let cases: &[(u64, &[u8])] = &[
        (37, &[0x25]),
        (11008, &[0x80, 0x56]),
        (526617, &[0x99, 0x92, 0x20]),
        (149225879, &[0x97, 0x83, 0x94, 0x47]),
        (13440780582, &[0xa6, 0x92, 0x88, 0x89, 0x32]),
        (623848604201, &[0xa9, 0x84, 0x99, 0x82, 0x94, 0x12]),
        (532167923073040, &[0x90, 0x80, 0x90, 0x80, 0x90, 0x80, 0x79]),
        (
            55818443594433425,
            &[0x91, 0x87, 0xf2, 0xb2, 0xc2, 0xd2, 0x93, 0x63],
        ),
        (
            136100349976529025,
            &[0x81, 0x91, 0xa1, 0xb1, 0xc1, 0xd1, 0xe1, 0xf1, 1],
        ),
    ];
    for &(value, encoded) in cases {
        assert_eq!(vli::decode(encoded), Ok((value, encoded.len())));
        let mut out = [0xa5; 12];
        assert_eq!(vli::encode(value, &mut out), encoded.len());
        assert_eq!(&out[..encoded.len()], encoded);
        assert!(out[encoded.len()..].iter().all(|&b| b == 0xa5));
        for end in 0..encoded.len() {
            assert_eq!(vli::decode(&encoded[..end]), Err(XzErrorKind::TruncatedVli));
        }
    }
    for bit in 1..63 {
        for value in [(1u64 << bit) - 1, 1u64 << bit, (1u64 << bit) + 1] {
            let mut out = [0; 9];
            let n = vli::encode(value, &mut out);
            assert_eq!(n, vli::encoded_len(value));
            assert_eq!(vli::decode(&out[..n]), Ok((value, n)));
        }
    }
    for n in 2..=9 {
        let mut noncanonical = vec![0x80; n];
        noncanonical[n - 1] = 0;
        assert_eq!(vli::decode(&noncanonical), Err(XzErrorKind::BadVli));
    }
}

fn header(flags: [u8; 2]) -> [u8; 12] {
    let mut out = [0; 12];
    out[..6].copy_from_slice(&[0xfd, b'7', b'z', b'X', b'Z', 0]);
    out[6..8].copy_from_slice(&flags);
    out[8..].copy_from_slice(&crc32(&flags).to_le_bytes());
    out
}

fn footer(flags: [u8; 2], backward: u32) -> [u8; 12] {
    let mut out = [0; 12];
    out[4..8].copy_from_slice(&backward.to_le_bytes());
    out[8..10].copy_from_slice(&flags);
    out[10..].copy_from_slice(b"YZ");
    let crc = crc32(&out[4..10]);
    out[..4].copy_from_slice(&crc.to_le_bytes());
    out
}

#[test]
fn upstream_stream_flags_checks_and_backward_size_boundaries() {
    // test_stream_flags.c and test_check.c: all IDs, reserved flags,
    // valid CRC versus damaged CRC, and maximum encoded backward size.
    for id in 0..16 {
        let flags = [0, id];
        let h = header(flags);
        assert_eq!(StreamHeader::parse(&h).unwrap().flags.raw, flags);
        let size = CheckType::from_id(id).unwrap().size();
        assert_eq!(
            size,
            [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64][id as usize]
        );
        for backward in [0, 1, (1 << 30) - 1, 1 << 30, u32::MAX] {
            let f = StreamFooter::parse(&footer(flags, backward)).unwrap();
            assert_eq!(f.index_size, (u64::from(backward) + 1) * 4);
            assert_eq!(f.flags.raw, flags);
        }
        for byte in 0..h.len() {
            for bit in 0..8 {
                let mut damaged = h;
                damaged[byte] ^= 1 << bit;
                assert!(StreamHeader::parse(&damaged).is_err());
            }
        }
    }
    for reserved in 1..=255 {
        assert!(StreamFlags::parse([reserved, 0]).is_err());
        assert!(StreamHeader::parse(&header([reserved, 0])).is_err());
        assert!(StreamFooter::parse(&footer([reserved, 0], 0)).is_err());
    }
    for reserved in 16..=255 {
        assert!(CheckType::from_id(reserved).is_err());
        assert!(StreamHeader::parse(&header([0, reserved])).is_err());
        assert!(StreamFooter::parse(&footer([0, reserved], 0)).is_err());
    }
}

fn index(records: &[(u64, u64)]) -> Vec<u8> {
    let mut out = vec![0];
    vli::push(records.len() as u64, &mut out);
    for &(packed, unpacked) in records {
        vli::push(packed, &mut out);
        vli::push(unpacked, &mut out);
    }
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

#[test]
fn upstream_index_sizes_cross_u32_and_reject_u64_wrap() {
    // test_index.c: offsets and totals must use the full VLI width.
    // This is structural coverage, not a claim to decode these payloads.
    let big = (1u64 << 32) + 1;
    let parsed = XzIndex::parse(&index(&[(big, big), (13, 17)])).unwrap();
    assert_eq!(parsed.uncompressed_size(), Some(big + 17));
    assert_eq!(parsed.blocks_size(), Some(((big + 3) & !3) + 16));
    let entries = parsed.blocks(big).unwrap();
    assert_eq!(entries[1].uncompressed_offset, big);
    assert_eq!(entries[1].file_offset, big + 12 + ((big + 3) & !3));
    assert!(parsed.blocks(u64::MAX - 12).is_none());
    assert!(XzIndex::parse(&index(&[(vli::VLI_MAX, 0); 3])).is_err());
    let overflow = XzIndex::parse(&index(&[(5, vli::VLI_MAX); 3])).unwrap();
    assert!(overflow.uncompressed_size().is_none());
    assert!(overflow.blocks(0).is_none());
    for invalid in 0..5 {
        assert!(XzIndex::parse(&index(&[(invalid, 0)])).is_err());
    }
}

#[test]
fn upstream_filter_property_sizes_and_reserved_ids() {
    use lzma_turbo::xz::{FilterChain, FilterFlags, bcj::BcjKind};
    // test_filter_flags.c deliberately separates syntactic parsing from
    // BCJ alignment validation: offset 257 parses even when misaligned.
    for kind in [
        BcjKind::X86,
        BcjKind::Ppc,
        BcjKind::Ia64,
        BcjKind::Arm,
        BcjKind::ArmThumb,
        BcjKind::Sparc,
        BcjKind::Arm64,
        BcjKind::RiscV,
    ] {
        for size in 0..=5 {
            let mut wire = Vec::new();
            vli::push(kind.filter_id(), &mut wire);
            vli::push(size, &mut wire);
            wire.extend_from_slice(&[1, 1, 0, 0, 0][..size as usize]);
            let parsed = FilterFlags::parse(&wire, &mut 0);
            if size == 0 || size == 4 {
                let flag = parsed.unwrap();
                let last = FilterFlags::new(0x21, &[0]).unwrap();
                assert_eq!(
                    FilterChain::validate(&[flag, last]).is_ok(),
                    size == 0 || 257 % kind.alignment() == 0
                );
                for end in 0..wire.len() {
                    assert!(FilterFlags::parse(&wire[..end], &mut 0).is_err());
                }
            } else {
                assert!(parsed.is_err());
            }
        }
    }
    for id in [0, 2, 0x20, 0x22, 1 << 62, vli::VLI_MAX] {
        let mut wire = Vec::new();
        vli::push(id, &mut wire);
        vli::push(vli::VLI_MAX, &mut wire);
        assert_eq!(
            FilterFlags::parse(&wire, &mut 0),
            Err(XzErrorKind::UnsupportedFilter)
        );
    }
    for id in [3, 0x21] {
        for size in [0, 2, 4, vli::VLI_MAX] {
            let mut wire = vec![id];
            vli::push(size, &mut wire);
            assert!(FilterFlags::parse(&wire, &mut 0).is_err());
        }
    }
}

#[cfg(feature = "enc")]
#[test]
fn upstream_bcj_exact_output_one_byte_input_and_empty_blocks() {
    use lzma_turbo::xz::{FilterFlags, bcj::BcjKind};
    use lzma_turbo::{LzmaEncProps, XzReader, encode_xz_with_filters};
    use std::io::{self, Read};
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.0.len()).min(1);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    let props = LzmaEncProps::new().with_level(1).with_dict_size(4096);
    // test_bcj_exact_size.c uses PowerPC and a 16-byte output. Extend the
    // same edge to all supported instruction widths and every short tail.
    for kind in [
        BcjKind::X86,
        BcjKind::Ppc,
        BcjKind::Ia64,
        BcjKind::Arm,
        BcjKind::ArmThumb,
        BcjKind::Sparc,
        BcjKind::Arm64,
        BcjKind::RiscV,
    ] {
        for len in 0..=17 {
            let source: Vec<u8> = (0..len).map(|n| b'0' + n as u8).collect();
            let filter = FilterFlags::new(kind.filter_id(), &[]).unwrap();
            let encoded =
                encode_xz_with_filters(&source, &props, CheckType::Crc32, 0, &[filter]).unwrap();
            let mut reader = XzReader::new(Trickle(&encoded));
            let mut output = vec![0; len];
            for byte in &mut output {
                reader.read_exact(std::slice::from_mut(byte)).unwrap();
            }
            assert_eq!(output, source);
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        }
    }
}

#[test]
fn upstream_block_header_optional_sizes_and_reserved_bits() {
    use lzma_turbo::xz::BlockHeader;
    let build = |flags: u8, fields: &[u8]| {
        let mut out = vec![0, flags];
        out.extend_from_slice(fields);
        while !(out.len() + 4).is_multiple_of(4) {
            out.push(0);
        }
        out[0] = ((out.len() + 4) / 4 - 1) as u8;
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    };
    // test_block_header.c: both optional size fields independently, multiple
    // filters, reserved flag bits and CRC-correct dirty padding.
    for packed in [None, Some(1), Some((1u64 << 32) + 1), Some(vli::VLI_MAX)] {
        for unpacked in [None, Some(0), Some((1u64 << 32) + 1), Some(vli::VLI_MAX)] {
            for converters in 0..=3u8 {
                let mut fields = Vec::new();
                let flags = converters
                    | if packed.is_some() { 0x40 } else { 0 }
                    | if unpacked.is_some() { 0x80 } else { 0 };
                if let Some(size) = packed {
                    vli::push(size, &mut fields);
                }
                if let Some(size) = unpacked {
                    vli::push(size, &mut fields);
                }
                for _ in 0..converters {
                    fields.extend_from_slice(&[3, 1, 0]);
                }
                fields.extend_from_slice(&[0x21, 1, 0]);
                let wire = build(flags, &fields);
                let parsed = BlockHeader::parse(&wire).unwrap();
                assert_eq!(parsed.compressed_size, packed);
                assert_eq!(parsed.uncompressed_size, unpacked);
                assert_eq!(parsed.chain.converters.len(), converters as usize);
                for bit in [4, 8, 16, 32] {
                    assert!(BlockHeader::parse(&build(flags | bit, &fields)).is_err());
                }
                for end in 0..wire.len() {
                    assert!(BlockHeader::parse(&wire[..end]).is_err());
                }
            }
        }
    }
    assert!(BlockHeader::parse(&build(0, &[0x21, 1, 0, 1])).is_err());
    assert!(BlockHeader::parse(&build(0x40, &[0, 0x21, 1, 0])).is_err());
}

#[cfg(feature = "enc")]
#[test]
fn upstream_index_hash_rejects_changed_records_even_with_matching_totals() {
    use lzma_turbo::{LzmaEncProps, XzOptions, XzParallelReader, XzReader, encode_xz};
    use std::io::{Cursor, Read};
    // test_index_hash.c checks that decoding the index agrees with records
    // appended while decoding blocks. These changes retain record count,
    // total unpacked size and a valid index CRC, isolating that comparison.
    let source: Vec<_> = (0..47u8).collect();
    let props = LzmaEncProps::new().with_level(1).with_dict_size(4096);
    let encoded = encode_xz(&source, &props, CheckType::Crc32, 16).unwrap();
    let footer_start = encoded.len() - 12;
    let footer = StreamFooter::parse(encoded[footer_start..].try_into().unwrap()).unwrap();
    let index_start = footer_start - footer.index_size as usize;
    let parsed = XzIndex::parse(&encoded[index_start..footer_start]).unwrap();
    assert_eq!(parsed.records.len(), 3);
    let records: Vec<_> = parsed
        .records
        .iter()
        .map(|r| (r.unpadded_size, r.uncompressed_size))
        .collect();
    let mut shifted = records.clone();
    shifted[0].1 -= 1;
    shifted[1].1 += 1;
    let mut reversed = records.clone();
    reversed.reverse();
    assert_ne!(records, reversed);
    for changed in [shifted, reversed] {
        let replacement = index(&changed);
        assert_eq!(replacement.len(), footer.index_size as usize);
        let mut damaged = encoded.clone();
        damaged[index_start..footer_start].copy_from_slice(&replacement);
        assert!(
            XzReader::new(&damaged[..])
                .read_to_end(&mut Vec::new())
                .is_err()
        );
        if let Ok(mut reader) = XzParallelReader::with_options(
            Cursor::new(&damaged),
            XzOptions::default().with_threads(2),
        ) {
            assert!(reader.read_to_end(&mut Vec::new()).is_err());
        }
    }
}
