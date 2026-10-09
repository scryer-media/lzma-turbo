//! LZMA and LZMA2 decoding, ported from the 7-Zip reference decoder.
//!
//! The decoder in this crate is a port of Igor Pavlov's `LzmaDec.c` and
//! `Lzma2Dec.c` from the LZMA SDK (public domain), kept structurally faithful
//! to the original so that its speed carries over: one large decode loop with
//! the range coder, probability base and dictionary pointers held in locals,
//! limits checked once per symbol under a caller-guaranteed margin, and a
//! separate careful path for the bytes near buffer edges.
//!
//! The encoder is a port of the same SDK's `LzFind.c`, `LzmaEnc.c` and
//! `Lzma2Enc.c`, and is bit-exact with it. See `docs/encoder.md`.
//!
//! # Example
//!
//! The reader adapters need `std`; without it the example below cannot run, so
//! it is compiled only when the `std` feature is on.
//!
#![cfg_attr(feature = "std", doc = "```no_run")]
#![cfg_attr(not(feature = "std"), doc = "```ignore")]
//! use std::fs::File;
//! use std::io::Read;
//! use lzma_turbo::LzmaReader;
//!
//! # fn main() -> std::io::Result<()> {
//! let mut out = Vec::new();
//! LzmaReader::new(File::open("archive.lzma")?)?.read_to_end(&mut out)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Provenance
//!
//! Derived from `C/LzmaDec.c` and `C/Lzma2Dec.c` of the LZMA SDK, which are in
//! the public domain. Each ported function names its C counterpart in a
//! comment.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs, rust_2018_idioms)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "enc")]
mod enc;
mod error;
#[cfg(feature = "filters")]
pub mod filters;
#[cfg(feature = "kernel-ab")]
mod kernel_ab;
mod lzma;
mod lzma2;
mod lzma_alone;

#[cfg(feature = "std")]
mod mt;
#[cfg(feature = "std")]
mod reader;

#[cfg(feature = "xz")]
pub mod xz;

#[cfg(feature = "enc")]
pub use enc::{
    BLOCK_SIZE_AUTO, BLOCK_SIZE_SOLID, LZMA_MATCH_LEN_MAX, LZMA_MATCH_LEN_MIN, Lzma2Encoder,
    Lzma2PushEncoder, Lzma2Writer, LzmaEncProps, LzmaEncoder, LzmaPushEncoder, LzmaWriter,
    MatchFinderKind, NormalizedProps, SeqInStream, SeqOutStream, SliceStream, auto_block_size,
    encode_lzma_alone, encode_lzma2, encode_lzma2_mt,
};
/// The `.xz` writer, behind the `xz` feature.
#[cfg(all(feature = "enc", feature = "xz"))]
pub use enc::{
    DEFAULT_BLOCK_SIZE, XzEncoder, XzWriter, encode_xz, encode_xz_mt, encode_xz_with_filters,
};
pub use error::{Error, FinishMode, Progress, Status, XzErrorKind};
pub use lzma::consts::{LZMA_PROPS_SIZE, LZMA_REQUIRED_INPUT_MAX};
pub use lzma::{LzmaDecoder, LzmaProps};
pub use lzma_alone::{LZMA_ALONE_HEADER_SIZE, LzmaAloneHeader};
pub use lzma2::Lzma2Decoder;
#[cfg(feature = "std")]
pub use lzma2::scan::run_boundaries;
pub use lzma2::scan::{Lzma2Run, Lzma2RunChunks, Lzma2RunScanner};

#[cfg(feature = "std")]
pub use mt::adaptive::{AdaptiveLedger, DrainStatus, Lzma2AdaptiveDecoder};
#[cfg(all(feature = "std", feature = "crc"))]
pub use mt::checksum;
#[cfg(all(feature = "std", feature = "crc"))]
pub use mt::checksum::{BlockChecks, Checksum, ChecksumPlan, Segment, SegmentCheck};
#[cfg(feature = "std")]
pub use mt::{Lzma2MtOptions, Lzma2ParallelDecoder, Lzma2ParallelReader, mt_memory_estimate};
#[cfg(feature = "std")]
pub use reader::{Lzma2Reader, LzmaReader};
#[cfg(feature = "xz")]
pub use xz::{XzAdaptiveDecoder, XzError, XzOptions, XzParallelReader, XzReader};

#[cfg(feature = "crc")]
pub mod crc;
#[cfg(any(feature = "crypto", feature = "native-crypto"))]
pub mod crypto;
#[cfg(any(feature = "crc-host", feature = "crypto-host"))]
pub mod hooks;

/// Whether this build decodes with the assembly loop ported from the LZMA
/// SDK's `Asm/` tree rather than with the portable Rust port of the C loop.
/// False without the `asm` feature and on every target that has no such loop.
pub const ASM_LOOP: bool = lzma::decode_opt::ENABLED;

/// Crate version, for consumers that record which decoder produced an output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
