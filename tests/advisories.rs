//! Inputs shaped after published LZMA and xz advisories. Each file is built
//! here byte by byte, out of stored LZMA2 chunks, so a case controls exactly
//! the field it lies about. Every reader must refuse it with an error: never
//! a panic, a hang, an allocation the input did not pay for, or output past
//! what the container declares.
#![cfg(feature = "xz")]

use std::io::{Cursor, Read};

use lzma_turbo::xz::{XzAdaptiveDecoder, XzOptions, XzParallelReader, XzReader};
use lzma_turbo::{DrainStatus, LzmaReader};

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xEDB8_8320 } else { 0 };
        }
    }
    !crc
}

fn vli(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// LZMA2 data of stored chunks: the first resets the dictionary unless
/// `first_control` says otherwise, the rest continue it.
fn stored_lzma2(payload: &[u8], first_control: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, chunk) in payload.chunks(1000).enumerate() {
        out.push(if index == 0 { first_control } else { 0x02 });
        out.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    out.push(0x00);
    out
}

/// One block of an `.xz` stream: what its header declares, its LZMA2 data,
/// and the sizes its index record gives.
struct Block {
    header_compressed: Option<u64>,
    header_uncompressed: Option<u64>,
    data: Vec<u8>,
    index_uncompressed: u64,
}

impl Block {
    /// An honest block: no sizes in the header, the true size in the index.
    fn honest(payload: &[u8]) -> Self {
        Self {
            header_compressed: None,
            header_uncompressed: None,
            data: stored_lzma2(payload, 0x01),
            index_uncompressed: payload.len() as u64,
        }
    }
}

/// An `.xz` stream with no integrity check over `blocks`, its index holding
/// one record per block.
fn xz_stream(blocks: &[Block]) -> Vec<u8> {
    let flags = [0x00, 0x00];
    let mut out = vec![0xFD, b'7', b'z', b'X', b'Z', 0x00];
    out.extend_from_slice(&flags);
    out.extend_from_slice(&crc32(&flags).to_le_bytes());
    let mut records = Vec::new();
    for block in blocks {
        let mut header = vec![0u8];
        header.push(
            u8::from(block.header_compressed.is_some()) << 6
                | u8::from(block.header_uncompressed.is_some()) << 7,
        );
        if let Some(size) = block.header_compressed {
            vli(size, &mut header);
        }
        if let Some(size) = block.header_uncompressed {
            vli(size, &mut header);
        }
        header.extend_from_slice(&[0x21, 0x01, 0x00]); // LZMA2, 4 KiB dictionary.
        while !(header.len() + 4).is_multiple_of(4) {
            header.push(0);
        }
        header[0] = ((header.len() + 4) / 4 - 1) as u8;
        let crc = crc32(&header);
        header.extend_from_slice(&crc.to_le_bytes());
        records.push((
            (header.len() + block.data.len()) as u64,
            block.index_uncompressed,
        ));
        out.extend_from_slice(&header);
        out.extend_from_slice(&block.data);
        pad4(&mut out);
    }
    let mut index = vec![0x00];
    vli(records.len() as u64, &mut index);
    for (unpadded, uncompressed) in records {
        vli(unpadded, &mut index);
        vli(uncompressed, &mut index);
    }
    pad4(&mut index);
    let crc = crc32(&index);
    index.extend_from_slice(&crc.to_le_bytes());
    let mut tail = ((index.len() / 4 - 1) as u32).to_le_bytes().to_vec();
    tail.extend_from_slice(&flags);
    out.extend_from_slice(&index);
    out.extend_from_slice(&crc32(&tail).to_le_bytes());
    out.extend_from_slice(&tail);
    out.extend_from_slice(b"YZ");
    out
}

fn sequential(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    XzReader::new(data).read_to_end(&mut out)?;
    Ok(out)
}

fn parallel(data: &[u8], threads: usize) -> std::io::Result<Vec<u8>> {
    let opts = XzOptions::default().with_threads(threads);
    let mut out = Vec::new();
    XzParallelReader::with_options(Cursor::new(data.to_vec()), opts)?.read_to_end(&mut out)?;
    Ok(out)
}

fn adaptive(data: &[u8], threads: usize) -> Result<Vec<u8>, String> {
    let mut decoder = XzAdaptiveDecoder::new(XzOptions::default().with_threads(threads));
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        if pos < data.len() {
            pos += decoder.feed(&data[pos..]).map_err(|e| e.to_string())?;
            if pos == data.len() {
                decoder.end_of_input();
            }
        }
        let status = decoder
            .drain(|offset, bytes| {
                let offset = usize::try_from(offset).unwrap();
                if out.len() < offset + bytes.len() {
                    out.resize(offset + bytes.len(), 0);
                }
                out[offset..offset + bytes.len()].copy_from_slice(bytes);
            })
            .map_err(|e| e.to_string())?;
        match status {
            DrainStatus::Finished => return Ok(out),
            DrainStatus::NeedsMoreInput if pos == data.len() => {
                return Err("wanted input after the end".into());
            }
            _ => {}
        }
    }
}

/// Every reader, at one thread and several, refuses `data`.
fn every_reader_refuses(data: &[u8], case: &str) {
    assert!(sequential(data).is_err(), "{case}: sequential");
    for threads in [1, 4] {
        assert!(
            parallel(data, threads).is_err(),
            "{case}: parallel, {threads} threads"
        );
        assert!(
            adaptive(data, threads).is_err(),
            "{case}: adaptive, {threads} threads"
        );
    }
}

/// Every reader, at one thread and several, decodes `data` to `expected`.
fn every_reader_decodes(data: &[u8], expected: &[u8], case: &str) {
    assert_eq!(sequential(data).unwrap(), expected, "{case}: sequential");
    for threads in [1, 4] {
        assert_eq!(
            parallel(data, threads).unwrap(),
            expected,
            "{case}: parallel"
        );
        assert_eq!(
            adaptive(data, threads).unwrap(),
            expected,
            "{case}: adaptive"
        );
    }
}

fn payload(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

#[test]
fn the_builder_produces_files_every_reader_accepts() {
    let a = payload(5000, 1);
    let b = payload(3001, 2);
    let file = xz_stream(&[Block::honest(&a), Block::honest(&b)]);
    every_reader_decodes(&file, &[a.as_slice(), &b].concat(), "honest");
    let mut sized = Block::honest(&a);
    sized.header_compressed = Some(sized.data.len() as u64);
    sized.header_uncompressed = Some(a.len() as u64);
    every_reader_decodes(
        &xz_stream(&[sized, Block::honest(&b)]),
        &[a.as_slice(), &b].concat(),
        "sized",
    );
}

/// CVE-2026-14266 (7-Zip, XZ chunked data overflows a heap buffer) and
/// CVE-2025-31115 (liblzma's threaded decoder): a decoder that sizes a
/// block's buffer from what the header or the index declares must stop when
/// the chunks carry more than that, at any thread count.
#[test]
fn chunks_carrying_more_than_the_block_declares_are_refused() {
    let data = payload(5000, 3);
    let tail = payload(2000, 4);
    for declared in [0u64, 1, 999, 1000, 4096, 4999] {
        let mut header = Block::honest(&data);
        header.header_uncompressed = Some(declared);
        header.index_uncompressed = declared;
        every_reader_refuses(
            &xz_stream(&[header, Block::honest(&tail)]),
            &format!("header and index say {declared}"),
        );

        let mut index = Block::honest(&data);
        index.index_uncompressed = declared;
        every_reader_refuses(
            &xz_stream(&[index, Block::honest(&tail)]),
            &format!("index says {declared}"),
        );

        let mut header_only = Block::honest(&data);
        header_only.header_uncompressed = Some(declared);
        every_reader_refuses(
            &xz_stream(&[header_only, Block::honest(&tail)]),
            &format!("header says {declared}"),
        );
    }
}

#[test]
fn chunks_carrying_less_than_the_block_declares_are_refused() {
    let data = payload(5000, 5);
    for declared in [5001u64, 6000, 1 << 20] {
        let mut both = Block::honest(&data);
        both.header_uncompressed = Some(declared);
        both.index_uncompressed = declared;
        every_reader_refuses(&xz_stream(&[both]), &format!("both say {declared}"));
        let mut index = Block::honest(&data);
        index.index_uncompressed = declared;
        every_reader_refuses(&xz_stream(&[index]), &format!("index says {declared}"));
    }
}

/// An index whose sizes no allocator could give must be refused from the
/// numbers alone, not by trying to allocate them.
#[test]
fn an_index_declaring_an_impossible_block_is_refused_before_allocating() {
    let data = payload(100, 6);
    for declared in [1u64 << 40, (1 << 62) - 1, (1 << 63) - 1] {
        let mut block = Block::honest(&data);
        block.index_uncompressed = declared;
        every_reader_refuses(&xz_stream(&[block]), &format!("index says {declared}"));
        let mut header = Block::honest(&data);
        header.header_uncompressed = Some(declared);
        header.index_uncompressed = declared;
        every_reader_refuses(&xz_stream(&[header]), &format!("both say {declared}"));
    }
}

/// CVE-2021-42374 (BusyBox unlzma reads before its buffer): a block's first
/// chunk must reset the dictionary. Accepting one that does not would let a
/// match reach before the block's first byte, where a threaded decoder's
/// per-block buffer has nothing.
#[test]
fn a_block_whose_first_chunk_keeps_the_dictionary_is_refused() {
    let data = payload(3000, 7);
    for control in [0x02, 0x80, 0xA0, 0xC0] {
        let mut block = Block::honest(&data);
        block.data = stored_lzma2(&data, control);
        let file = xz_stream(&[Block::honest(&payload(2000, 8)), block]);
        every_reader_refuses(&file, &format!("first control {control:#04x}"));
    }
}

/// CVE-2026-34743 (liblzma, an index with no records left in a state that
/// overflowed on the next append): a stream with no blocks is valid, decodes
/// to nothing, and the stream after it decodes as if it were alone.
#[test]
fn a_stream_with_no_blocks_decodes_and_so_does_the_next() {
    let empty = xz_stream(&[]);
    every_reader_decodes(&empty, b"", "empty stream");
    let data = payload(4000, 9);
    let mut file = empty.clone();
    file.extend_from_slice(&xz_stream(&[Block::honest(&data)]));
    file.extend_from_slice(&empty);
    file.extend_from_slice(&xz_stream(&[Block::honest(&data)]));
    every_reader_decodes(
        &file,
        &[data.as_slice(), &data].concat(),
        "empty streams between",
    );
}

/// CVE-2021-29482 (ulikunitz/xz's unbounded varint loop): an index record
/// count longer than nine bytes is refused.
#[test]
fn an_index_count_longer_than_nine_bytes_is_refused() {
    let mut file = xz_stream(&[]);
    // The empty index is 0x00, a one-byte count, two padding bytes and the CRC.
    let index_at = file.len() - 12 - 8;
    let mut index = vec![0x00];
    index.extend_from_slice(&[0xFF; 10]);
    index.push(0x01);
    pad4(&mut index);
    let crc = crc32(&index);
    index.extend_from_slice(&crc.to_le_bytes());
    let mut tail = ((index.len() / 4 - 1) as u32).to_le_bytes().to_vec();
    tail.extend_from_slice(&[0x00, 0x00]);
    file.truncate(index_at);
    file.extend_from_slice(&index);
    file.extend_from_slice(&crc32(&tail).to_le_bytes());
    file.extend_from_slice(&tail);
    file.extend_from_slice(b"YZ");
    every_reader_refuses(&file, "ten-byte count");
}

/// CVE-2018-9251 and CVE-2018-14567 (libxml2 looping on a liblzma error): a
/// reader that has failed keeps failing; it never reports a clean end or
/// hands out more bytes.
#[test]
fn a_failed_reader_keeps_failing() {
    let data = payload(5000, 10);
    let mut block = Block::honest(&data);
    block.header_uncompressed = Some(1000);
    block.index_uncompressed = 1000;
    let file = xz_stream(&[block]);
    let mut buffer = [0u8; 256];
    let mut reader = XzReader::new(file.as_slice());
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
    for _ in 0..3 {
        assert!(reader.read(&mut buffer).is_err(), "sequential");
    }
    for threads in [1, 4] {
        let opts = XzOptions::default().with_threads(threads);
        let mut reader = XzParallelReader::with_options(Cursor::new(file.clone()), opts).unwrap();
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        for _ in 0..3 {
            assert!(
                reader.read(&mut buffer).is_err(),
                "parallel, {threads} threads"
            );
        }
    }
}

/// CVE-2025-58058 (ulikunitz/xz allocating the dictionary a junk `.lzma`
/// header names) and CVE-2017-18258 (libxml2 not bounding liblzma's memory):
/// the default `.lzma` reader refuses a header whose dictionary is past its
/// limit before allocating anything.
#[test]
fn a_lzma_header_naming_a_huge_dictionary_is_refused_by_the_default_limit() {
    for dictionary in [u32::MAX, 0xF000_0000, 1 << 30] {
        let mut header = vec![0x5D];
        header.extend_from_slice(&dictionary.to_le_bytes());
        header.extend_from_slice(&u64::MAX.to_le_bytes());
        header.extend_from_slice(&[0; 32]);
        let error = LzmaReader::new(header.as_slice()).err().expect("refused");
        assert!(
            error.to_string().contains("limit"),
            "{dictionary:#x}: {error}"
        );
    }
}

/// CVE-2021-28211 (EDK II sized its output from the `.lzma` header and the
/// stream wrote past it): a stream carrying more than its header declares
/// is refused, and nothing past the declared size is handed out.
#[test]
fn a_lzma_stream_longer_than_its_header_declares_is_refused() {
    for name in ["tiny.p9e.lzma", "mixed.p1.lzma"] {
        let file = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(name),
        )
        .unwrap();
        let mut whole = Vec::new();
        LzmaReader::new(file.as_slice())
            .unwrap()
            .read_to_end(&mut whole)
            .unwrap();
        assert!(whole.len() > 2, "{name}");
        for declared in [0, 1, whole.len() as u64 / 2, whole.len() as u64 - 1] {
            let mut lying = file.clone();
            lying[5..13].copy_from_slice(&declared.to_le_bytes());
            let mut reader = LzmaReader::new(lying.as_slice()).unwrap();
            let mut out = Vec::new();
            let mut buffer = [0u8; 97];
            let error = loop {
                match reader.read(&mut buffer) {
                    Ok(0) => panic!("{name}, declared {declared}: a clean end"),
                    Ok(n) => out.extend_from_slice(&buffer[..n]),
                    Err(error) => break error,
                }
            };
            assert!(
                out.len() as u64 <= declared,
                "{name}, declared {declared}: {error}"
            );
            assert_eq!(out, whole[..out.len()], "{name}, declared {declared}");
            assert!(
                reader.read(&mut buffer).is_err(),
                "{name}, declared {declared}: retried"
            );
        }
    }
}
