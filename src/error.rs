//! Error, status and progress types.
//!
//! These mirror the reference decoder's `SRes` codes (`SZ_ERROR_DATA`,
//! `SZ_ERROR_UNSUPPORTED`, `SZ_ERROR_FAIL`) and its `ELzmaStatus` /
//! `ELzmaFinishMode` enumerations from `C/LzmaDec.h`.

use core::fmt;

/// Why an xz read failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XzErrorKind {
    /// The stream header's magic bytes are not `FD 37 7A 58 5A 00`.
    BadMagic,
    /// The footer's magic bytes are not `YZ`.
    BadFooterMagic,
    /// A reserved bit is set in the stream flags, or the first flag byte is
    /// not zero.
    BadStreamFlags,
    /// The stream footer's flags do not match the header's.
    StreamFlagsMismatch,
    /// A CRC-32 over a header, footer or index did not match.
    HeaderCrc,
    /// The stream's check type is one this build cannot compute, and the
    /// caller did not allow an unverified decode.
    UnsupportedCheck,
    /// The check stored for a block does not match what the block decoded to.
    CheckMismatch,
    /// A filter id the crate does not implement.
    UnsupportedFilter,
    /// A filter chain the format does not allow: too many filters, a
    /// non-last filter last, a last filter used as a non-last one, a repeated
    /// filter, or properties of the wrong size.
    BadFilterChain,
    /// A variable-length integer that is too long, too wide, or not the
    /// shortest encoding of its value.
    BadVli,
    /// Input ended inside a variable-length integer.
    TruncatedVli,
    /// A padding field contained something other than null bytes, or was the
    /// wrong length.
    BadPadding,
    /// A block header's size byte, flags or fields are out of range.
    BadBlockHeader,
    /// A size declared in a block header or in the index does not match what
    /// was actually read or produced.
    SizeMismatch,
    /// The index does not describe the blocks that were decoded: the record
    /// count, a record's sizes, or the position the index sits at.
    IndexMismatch,
    /// Input ended before the structure being read was complete.
    TruncatedInput,
    /// Bytes follow the last stream that are neither stream padding nor
    /// another stream.
    TrailingGarbage,
    /// Honouring the stream would need more memory than the caller allowed.
    /// Carries what was needed.
    #[non_exhaustive]
    MemoryLimit {
        /// Bytes the stream asks for.
        needed: u64,
        /// Bytes the caller allowed.
        limit: u64,
    },
    /// The stream decodes to more than the caller's `max_unpack_bytes`.
    #[non_exhaustive]
    TooMuchOutput {
        /// The cap that was exceeded.
        limit: u64,
    },
    /// The LZMA2 layer failed: corrupt or truncated data, an allocation, a
    /// cancellation. The [`Error`] is this kind's `source()`.
    Lzma(Error),
}

impl XzErrorKind {
    /// [`XzErrorKind::MemoryLimit`]: `needed` bytes asked for, `limit` allowed.
    #[must_use]
    pub const fn memory_limit(needed: u64, limit: u64) -> Self {
        XzErrorKind::MemoryLimit { needed, limit }
    }

    /// [`XzErrorKind::TooMuchOutput`] over a cap of `limit` bytes.
    #[must_use]
    pub const fn too_much_output(limit: u64) -> Self {
        XzErrorKind::TooMuchOutput { limit }
    }

    /// The LZMA2 layer's error, if that is where the failure came from.
    #[must_use]
    pub const fn lzma(&self) -> Option<Error> {
        match self {
            XzErrorKind::Lzma(e) => Some(*e),
            _ => None,
        }
    }

    /// The `io::ErrorKind` of this failure's class. An LZMA2 failure keeps the
    /// class of its [`Error`]; a cap on the output is a refusal of the data.
    #[cfg(feature = "xz")]
    pub(crate) const fn io_kind(&self) -> std::io::ErrorKind {
        use std::io::ErrorKind;
        match self {
            XzErrorKind::TruncatedInput | XzErrorKind::TruncatedVli => ErrorKind::UnexpectedEof,
            XzErrorKind::MemoryLimit { .. } => ErrorKind::OutOfMemory,
            XzErrorKind::UnsupportedCheck | XzErrorKind::UnsupportedFilter => {
                ErrorKind::Unsupported
            }
            XzErrorKind::Lzma(e) => e.io_kind(),
            XzErrorKind::BadMagic
            | XzErrorKind::BadFooterMagic
            | XzErrorKind::BadStreamFlags
            | XzErrorKind::StreamFlagsMismatch
            | XzErrorKind::HeaderCrc
            | XzErrorKind::CheckMismatch
            | XzErrorKind::BadFilterChain
            | XzErrorKind::BadVli
            | XzErrorKind::BadPadding
            | XzErrorKind::BadBlockHeader
            | XzErrorKind::SizeMismatch
            | XzErrorKind::IndexMismatch
            | XzErrorKind::TrailingGarbage
            | XzErrorKind::TooMuchOutput { .. } => ErrorKind::InvalidData,
        }
    }
}

impl fmt::Display for XzErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XzErrorKind::BadMagic => f.write_str("not an xz stream"),
            XzErrorKind::BadFooterMagic => f.write_str("bad xz stream footer magic"),
            XzErrorKind::BadStreamFlags => f.write_str("bad xz stream flags"),
            XzErrorKind::StreamFlagsMismatch => {
                f.write_str("xz stream footer flags differ from the header's")
            }
            XzErrorKind::HeaderCrc => f.write_str("xz header CRC-32 mismatch"),
            XzErrorKind::UnsupportedCheck => f.write_str("unsupported xz check type"),
            XzErrorKind::CheckMismatch => f.write_str("xz block check mismatch"),
            XzErrorKind::UnsupportedFilter => f.write_str("unsupported xz filter"),
            XzErrorKind::BadFilterChain => f.write_str("invalid xz filter chain"),
            XzErrorKind::BadVli => f.write_str("invalid xz variable-length integer"),
            XzErrorKind::TruncatedVli => {
                f.write_str("xz input ended inside a variable-length integer")
            }
            XzErrorKind::BadPadding => f.write_str("xz padding is not null bytes"),
            XzErrorKind::BadBlockHeader => f.write_str("invalid xz block header"),
            XzErrorKind::SizeMismatch => f.write_str("xz size field does not match the data"),
            XzErrorKind::IndexMismatch => f.write_str("xz index does not match the blocks"),
            XzErrorKind::TruncatedInput => f.write_str("truncated xz input"),
            XzErrorKind::TrailingGarbage => f.write_str("trailing garbage after the xz stream"),
            XzErrorKind::MemoryLimit { needed, limit } => write!(
                f,
                "xz stream needs {needed} bytes, over the {limit}-byte limit"
            ),
            XzErrorKind::TooMuchOutput { limit } => {
                write!(f, "xz stream decodes to more than the {limit}-byte cap")
            }
            XzErrorKind::Lzma(_) => f.write_str("xz LZMA2 layer failed"),
        }
    }
}

impl core::error::Error for XzErrorKind {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            XzErrorKind::Lzma(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for XzErrorKind {
    fn from(e: Error) -> Self {
        XzErrorKind::Lzma(e)
    }
}

/// Reasons a decode or encode call can fail.
///
/// C: the `SZ_ERROR_*` subset that `LzmaDec.c`, `Lzma2Dec.c`, `LzmaEnc.c` and
/// `Lzma2Enc.c` can return, plus the failures the reference leaves to its
/// caller: truncated input, a memory budget, cancellation.
///
/// With `std`, `io::Error::from` maps each variant to the [`std::io::ErrorKind`]
/// of its class and keeps the value as the payload, so
/// `io::Error::downcast::<Error>()` gives it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// C: `SZ_ERROR_UNSUPPORTED`. The property byte(s) do not describe a
    /// stream this decoder can handle. `io::ErrorKind::Unsupported`.
    UnsupportedProps,
    /// C: `SZ_ERROR_DATA`. The compressed stream is malformed: a match refers
    /// outside the dictionary, a range coder check failed, the end marker was
    /// absent where one was required, or an LZMA2 chunk header is invalid.
    /// `io::ErrorKind::InvalidData`.
    CorruptData,
    /// [`Error::CorruptData`], located. `io::ErrorKind::InvalidData`.
    ///
    /// Only the multi-threaded decoder produces this: a worker decodes one
    /// independently decodable run and so can say which one failed and where
    /// its output would have gone, which a caller writing blocks by offset
    /// needs in order to know what it has and has not got.
    #[non_exhaustive]
    CorruptRun {
        /// The run's index in the stream, counting from zero.
        index: u64,
        /// Where the run's output starts, in bytes from the start of the
        /// decoded stream.
        out_offset: u64,
    },
    /// The input ended before the stream did. `io::ErrorKind::UnexpectedEof`.
    Truncated,
    /// C: `SZ_ERROR_FAIL`. Reached only if the coder's own invariants are
    /// violated; it is never produced by malformed input alone.
    /// `io::ErrorKind::Other`.
    InternalFailure,
    /// An allocation the coder needed failed, or a worker thread could not be
    /// started. `io::ErrorKind::OutOfMemory`.
    Alloc,
    /// Decoding would need more memory than the caller allowed.
    /// `io::ErrorKind::OutOfMemory`.
    #[non_exhaustive]
    MemoryLimit {
        /// Bytes the stream asks for.
        needed: u64,
        /// Bytes the caller allowed.
        limit: u64,
    },
    /// The caller cancelled the decode. `io::ErrorKind::Other`, never
    /// `Interrupted`, which std's read and copy helpers retry: a cancelled
    /// read would spin instead of stop.
    Cancelled,
    /// C: `SZ_ERROR_PARAM`. An argument from the caller is out of range: an
    /// encoder setting, a range outside its buffer, a call out of sequence,
    /// a stream that broke the length it promised or a sink that reported
    /// more than it was given. `io::ErrorKind::InvalidInput`.
    Param,
    /// C: `SZ_ERROR_READ`. The encoder's input stream failed. The stream's
    /// own error stays with the stream's owner, which is where a
    /// `SeqInStream` keeps it. `io::ErrorKind::Other`.
    Read,
    /// C: `SZ_ERROR_WRITE`. The encoder's output stream failed. The stream's
    /// own error stays with the stream's owner, which is where a
    /// `SeqOutStream` keeps it. `io::ErrorKind::Other`.
    Write,
}

impl Error {
    /// [`Error::CorruptRun`] for run `index`, whose output starts at
    /// `out_offset`.
    #[must_use]
    pub const fn corrupt_run(index: u64, out_offset: u64) -> Self {
        Error::CorruptRun { index, out_offset }
    }

    /// [`Error::MemoryLimit`]: `needed` bytes asked for, `limit` allowed.
    #[must_use]
    pub const fn memory_limit(needed: u64, limit: u64) -> Self {
        Error::MemoryLimit { needed, limit }
    }

    /// Whether the input is at fault: [`Error::CorruptData`],
    /// [`Error::CorruptRun`] or [`Error::Truncated`].
    #[must_use]
    pub const fn is_corrupt(&self) -> bool {
        matches!(
            self,
            Error::CorruptData | Error::CorruptRun { .. } | Error::Truncated
        )
    }

    /// The `io::ErrorKind` of this error's class.
    #[cfg(feature = "std")]
    pub(crate) const fn io_kind(&self) -> std::io::ErrorKind {
        use std::io::ErrorKind;
        match self {
            Error::UnsupportedProps => ErrorKind::Unsupported,
            Error::CorruptData | Error::CorruptRun { .. } => ErrorKind::InvalidData,
            Error::Truncated => ErrorKind::UnexpectedEof,
            Error::Alloc | Error::MemoryLimit { .. } => ErrorKind::OutOfMemory,
            Error::Param => ErrorKind::InvalidInput,
            Error::Cancelled | Error::InternalFailure | Error::Read | Error::Write => {
                ErrorKind::Other
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Error::UnsupportedProps => "unsupported LZMA properties",
            Error::CorruptData => "corrupt LZMA data",
            Error::CorruptRun { index, out_offset } => {
                return write!(
                    f,
                    "corrupt LZMA2 data in run {index}, at output offset {out_offset}"
                );
            }
            Error::Truncated => "truncated LZMA stream",
            Error::InternalFailure => "internal LZMA coder failure",
            Error::Alloc => "LZMA coder allocation failed",
            Error::MemoryLimit { needed, limit } => {
                return write!(
                    f,
                    "LZMA decoder needs {needed} bytes, over the {limit}-byte limit"
                );
            }
            Error::Cancelled => "LZMA decode cancelled",
            Error::Param => "invalid LZMA coder argument",
            Error::Read => "LZMA encoder input stream failed",
            Error::Write => "LZMA encoder output stream failed",
        };
        f.write_str(s)
    }
}

impl core::error::Error for Error {}

#[cfg(feature = "std")]
impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        std::io::Error::new(e.io_kind(), e)
    }
}

/// Where a decode call stopped.
///
/// C: `ELzmaStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    /// C: `LZMA_STATUS_NOT_SPECIFIED`. No useful status; the caller should use
    /// the returned counts instead.
    NotSpecified,
    /// C: `LZMA_STATUS_FINISHED_WITH_MARK`. The stream ended with an end
    /// marker.
    FinishedWithMark,
    /// C: `LZMA_STATUS_NOT_FINISHED`. The output limit was reached and the
    /// stream continues.
    NotFinished,
    /// C: `LZMA_STATUS_NEEDS_MORE_INPUT`. All input was consumed mid-symbol.
    NeedsMoreInput,
    /// C: `LZMA_STATUS_MAYBE_FINISHED_WITHOUT_MARK`. The output limit was
    /// reached with the range coder in a state that could be a clean end of a
    /// stream that carries no end marker.
    MaybeFinishedWithoutMark,
}

/// Whether the current output buffer is expected to cover the end of the
/// stream.
///
/// C: `ELzmaFinishMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishMode {
    /// C: `LZMA_FINISH_ANY`. Decode at most the bytes that fit; do not look
    /// ahead for an end marker.
    Any,
    /// C: `LZMA_FINISH_END`. The block must be finished when the output limit
    /// is reached.
    End,
}

/// What one decode call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes consumed from the input slice.
    pub read: usize,
    /// Bytes produced into the output slice.
    pub written: usize,
    /// Where the call stopped.
    pub status: Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    /// Every message in a `source()` chain, outermost first.
    fn chain(e: &(dyn core::error::Error + 'static)) -> Vec<String> {
        let mut out = Vec::new();
        let mut next = Some(e);
        while let Some(e) = next {
            out.push(e.to_string());
            next = e.source();
        }
        out
    }

    #[test]
    fn the_error_types_cross_threads_and_live_forever() {
        fn assert_bounds<T: core::error::Error + Send + Sync + 'static>() {}
        assert_bounds::<Error>();
        assert_bounds::<XzErrorKind>();
    }

    #[test]
    fn an_lzma_error_is_its_own_whole_chain() {
        assert_eq!(chain(&Error::CorruptData), ["corrupt LZMA data"]);
        assert_eq!(
            chain(&Error::corrupt_run(3, 4096)),
            ["corrupt LZMA2 data in run 3, at output offset 4096"]
        );
        assert_eq!(
            chain(&Error::memory_limit(1 << 20, 4096)),
            ["LZMA decoder needs 1048576 bytes, over the 4096-byte limit"]
        );
    }

    #[test]
    fn an_xz_kind_reports_its_lzma_cause_once() {
        assert_eq!(
            chain(&XzErrorKind::Lzma(Error::Truncated)),
            ["xz LZMA2 layer failed", "truncated LZMA stream"]
        );
        assert_eq!(
            chain(&XzErrorKind::HeaderCrc),
            ["xz header CRC-32 mismatch"]
        );
        assert_eq!(XzErrorKind::Lzma(Error::Alloc).lzma(), Some(Error::Alloc));
        assert_eq!(XzErrorKind::BadMagic.lzma(), None);
    }

    #[test]
    fn only_the_input_is_corrupt() {
        assert!(Error::CorruptData.is_corrupt());
        assert!(Error::corrupt_run(0, 0).is_corrupt());
        assert!(Error::Truncated.is_corrupt());
        for e in [
            Error::UnsupportedProps,
            Error::InternalFailure,
            Error::Alloc,
            Error::memory_limit(2, 1),
            Error::Cancelled,
            Error::Param,
            Error::Read,
            Error::Write,
        ] {
            assert!(!e.is_corrupt(), "{e:?}");
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn an_io_error_has_the_class_kind_and_gives_the_error_back() {
        use std::io::ErrorKind;
        for (e, kind) in [
            (Error::UnsupportedProps, ErrorKind::Unsupported),
            (Error::CorruptData, ErrorKind::InvalidData),
            (Error::corrupt_run(1, 2), ErrorKind::InvalidData),
            (Error::Truncated, ErrorKind::UnexpectedEof),
            (Error::InternalFailure, ErrorKind::Other),
            (Error::Alloc, ErrorKind::OutOfMemory),
            (Error::memory_limit(2, 1), ErrorKind::OutOfMemory),
            (Error::Cancelled, ErrorKind::Other),
            (Error::Param, ErrorKind::InvalidInput),
            (Error::Read, ErrorKind::Other),
            (Error::Write, ErrorKind::Other),
        ] {
            let io = std::io::Error::from(e);
            assert_eq!(io.kind(), kind, "{e:?}");
            assert_eq!(chain(&io), [e.to_string()]);
            assert_eq!(io.downcast::<Error>().ok(), Some(e));
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_readers_fail_with_the_typed_error() {
        use std::io::{ErrorKind, Read};

        let err = crate::Lzma2Reader::with_memory_limit(&[][..], 16, 1)
            .err()
            .expect("over the limit");
        assert_eq!(err.kind(), ErrorKind::OutOfMemory);
        assert!(matches!(
            err.downcast::<Error>(),
            Ok(Error::MemoryLimit { limit: 1, .. })
        ));

        let mut out = Vec::new();
        let err = crate::Lzma2Reader::new(&[][..], 16)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(err.downcast::<Error>().ok(), Some(Error::Truncated));

        // A `.lzma` header the source ends inside.
        let err = crate::LzmaReader::new(&[0x5D, 0, 0, 1][..])
            .err()
            .expect("a short header");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(err.downcast::<Error>().ok(), Some(Error::Truncated));

        let err = crate::Lzma2Reader::new(&[][..], 41)
            .err()
            .expect("dict_prop 41");
        assert_eq!(err.kind(), ErrorKind::Unsupported);
        assert_eq!(err.downcast::<Error>().ok(), Some(Error::UnsupportedProps));
    }

    #[cfg(feature = "xz")]
    #[test]
    fn an_xz_error_names_its_place_and_returns_its_cause() {
        use crate::XzError;
        use std::io::ErrorKind;

        let located = XzError::in_block(Error::CorruptData, 0, 2, 64);
        let want = [
            "xz LZMA2 layer failed (stream 0, block 2, file offset 64)",
            "corrupt LZMA data",
        ];
        assert_eq!(chain(&located), want);
        let io = std::io::Error::from(located);
        assert_eq!(io.kind(), ErrorKind::InvalidData);
        assert_eq!(chain(&io), want);
        assert_eq!(io.downcast::<XzError>().ok(), Some(located));

        for (kind, io_kind) in [
            (XzErrorKind::TruncatedInput, ErrorKind::UnexpectedEof),
            (XzErrorKind::memory_limit(2, 1), ErrorKind::OutOfMemory),
            (XzErrorKind::UnsupportedFilter, ErrorKind::Unsupported),
            (XzErrorKind::too_much_output(9), ErrorKind::InvalidData),
            (XzErrorKind::Lzma(Error::Cancelled), ErrorKind::Other),
            (XzErrorKind::Lzma(Error::Alloc), ErrorKind::OutOfMemory),
            (XzErrorKind::Lzma(Error::InternalFailure), ErrorKind::Other),
        ] {
            let io = std::io::Error::from(XzError::at(kind, 1, 12));
            assert_eq!(io.kind(), io_kind, "{kind:?}");
        }
        assert_eq!(
            chain(&XzError::at(XzErrorKind::BadMagic, 1, 0)),
            ["not an xz stream (stream 1, file offset 0)"]
        );
    }
}
