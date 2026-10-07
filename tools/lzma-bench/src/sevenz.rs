//! Finding the raw LZMA2 stream inside a `.7z` file.
//!
//! Not a 7z reader, and deliberately not one: 7z is out of scope for this
//! crate (see `docs/porting.md`). This is the smallest walk that answers one
//! question about one shape of archive — a single file in a single folder
//! compressed with a single LZMA2 coder, which is what
//! `7zz a -mx5 -m0=lzma2` produces from one input file — namely "which bytes
//! are the LZMA2 stream, and what is its dictionary property byte?".
//!
//! It is shared by the benchmark harness and the large-fixture tests, and
//! lives here rather than in the library because nothing in the library may
//! know what a `.7z` file is.
//!
//! The alternative was to read `7zz l -slt`'s "Packed Size" and its
//! `Method = LZMA2:25` string. Parsing the header is preferred because it
//! needs no `7zz` on the machine, cannot be confused by a localised or
//! reformatted listing, and gives the property byte itself rather than a
//! rounded dictionary size that has to be mapped back to one.

#![allow(dead_code)]

/// Where the LZMA2 stream is in a `.7z` file, and how to decode it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackStream {
    /// Offset of the first byte of the LZMA2 stream. Always 32: the packed
    /// streams follow the signature header immediately.
    pub offset: u64,
    /// The LZMA2 stream's length in the file.
    pub packed_len: u64,
    /// What it decodes to, from the folder's unpacked size.
    pub unpacked_len: u64,
    /// The LZMA2 coder's one property byte: the dictionary size.
    pub dict_prop: u8,
}

// The header property ids this walk understands. C: `EIdEnum` in `C/7zIn.h`.
const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SIZE: u8 = 0x09;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_ENCODED_HEADER: u8 = 0x17;

const LZMA2_CODER_ID: u64 = 0x21;

/// Reads the pack-stream range and dictionary property byte out of `data`.
///
/// Returns `None` for anything that is not the one shape described above,
/// including an archive whose header is itself compressed (`kEncodedHeader`),
/// which `7zz` only produces for archives with more than a couple of files.
pub fn pack_stream(data: &[u8]) -> Option<PackStream> {
    if data.len() < 32 || &data[..6] != b"7z\xBC\xAF\x27\x1C" {
        return None;
    }
    let (start, len) = header_range(data[..32].try_into().ok()?)?;
    let start = usize::try_from(start).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    pack_stream_in_header(data.get(start..end)?)
}

/// Where the archive's header is, from its 32-byte signature header: the
/// absolute offset and the length. This is what lets a caller find the pack
/// stream by reading the two ends of the file rather than all of it.
pub fn header_range(signature: &[u8; 32]) -> Option<(u64, u64)> {
    if &signature[..6] != b"7z\xBC\xAF\x27\x1C" {
        return None;
    }
    let next_offset = u64::from_le_bytes(signature[12..20].try_into().ok()?);
    let next_size = u64::from_le_bytes(signature[20..28].try_into().ok()?);
    Some((32u64.checked_add(next_offset)?, next_size))
}

/// [`pack_stream`] given the header bytes alone, as [`header_range`] locates
/// them.
pub fn pack_stream_in_header(header: &[u8]) -> Option<PackStream> {
    let mut r = Reader {
        buf: header,
        pos: 0,
    };
    match r.byte()? {
        K_HEADER => {}
        // A compressed header would have to be decoded before it could be
        // parsed. Deliberately unsupported: the fixtures do not have one, and
        // supporting it is how a helper turns into a 7z reader.
        K_ENCODED_HEADER => return None,
        _ => return None,
    }
    if r.byte()? != K_MAIN_STREAMS_INFO {
        return None;
    }

    // PackInfo: base position, one pack stream, its size.
    if r.byte()? != K_PACK_INFO {
        return None;
    }
    let pack_pos = r.number()?;
    if r.number()? != 1 {
        return None;
    }
    if r.byte()? != K_SIZE {
        return None;
    }
    let packed_len = r.number()?;
    if r.byte()? != K_END {
        return None;
    }

    // UnPackInfo: one folder, one coder, and that coder is LZMA2.
    if r.byte()? != K_UNPACK_INFO || r.byte()? != K_FOLDER {
        return None;
    }
    if r.number()? != 1 {
        return None;
    }
    if r.byte()? != 0 {
        return None; // folder definitions stored elsewhere
    }
    if r.number()? != 1 {
        return None; // more than one coder is a chain this cannot follow
    }
    let flags = r.byte()?;
    let id_size = usize::from(flags & 0x0F);
    if flags & 0x10 != 0 {
        return None; // complex coder: several in or out streams
    }
    let mut id: u64 = 0;
    for _ in 0..id_size {
        id = (id << 8) | u64::from(r.byte()?);
    }
    if id != LZMA2_CODER_ID {
        return None;
    }
    if flags & 0x20 == 0 {
        return None; // LZMA2 always carries its dictionary property byte
    }
    if r.number()? != 1 {
        return None;
    }
    let dict_prop = r.byte()?;

    if r.byte()? != K_CODERS_UNPACK_SIZE {
        return None;
    }
    let unpacked_len = r.number()?;

    Some(PackStream {
        offset: 32 + pack_pos,
        packed_len,
        unpacked_len,
        dict_prop,
    })
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// C: `ReadNumber` in `C/7zIn.c`. The top bits of the first byte say how
    /// many more bytes follow, little-endian, and whatever is left of the
    /// first byte is the most significant part.
    fn number(&mut self) -> Option<u64> {
        let first = self.byte()?;
        let mut mask = 0x80u8;
        let mut value = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                let high = u64::from(first & (mask.wrapping_sub(1)));
                return Some(value | (high << (i * 8)));
            }
            value |= u64::from(self.byte()?) << (i * 8);
            mask >>= 1;
        }
        Some(value)
    }
}
