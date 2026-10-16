//! What can go wrong reading an `.xz` file, and where.
//!
//! Every error carries the stream and block it happened in and the file
//! offset, because a caller that has already written some output needs to
//! know how much of it is good. C: `XzDec.c` returns bare `SZ_ERROR_DATA` and
//! leaves the caller to guess; this does not copy that.

use core::fmt;

pub use crate::error::XzErrorKind;

/// An xz read failure, located.
///
/// `Display` renders the kind and the location; `source()` is the kind's own
/// source, the LZMA2 [`crate::Error`] behind an [`XzErrorKind::Lzma`]. Build
/// one with [`XzError::at`] or [`XzError::in_block`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct XzError {
    /// What went wrong.
    pub kind: XzErrorKind,
    /// Which stream of a concatenated file, counting from zero.
    pub stream: u64,
    /// Which block of that stream, counting from zero, if the failure
    /// belongs to one.
    pub block: Option<u64>,
    /// Offset in the file where the failure was noticed.
    pub offset: u64,
}

impl XzError {
    /// An error at `offset` in stream `stream`, outside any block.
    #[must_use]
    pub fn at(kind: impl Into<XzErrorKind>, stream: u64, offset: u64) -> Self {
        XzError {
            kind: kind.into(),
            stream,
            block: None,
            offset,
        }
    }

    /// An error inside block `block` of stream `stream`.
    #[must_use]
    pub fn in_block(kind: impl Into<XzErrorKind>, stream: u64, block: u64, offset: u64) -> Self {
        XzError {
            kind: kind.into(),
            stream,
            block: Some(block),
            offset,
        }
    }
}

impl fmt::Display for XzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        match self.block {
            Some(b) => write!(
                f,
                " (stream {}, block {}, file offset {})",
                self.stream, b, self.offset
            ),
            None => write!(f, " (stream {}, file offset {})", self.stream, self.offset),
        }
    }
}

impl core::error::Error for XzError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        // The kind is rendered above, so its source is this error's.
        core::error::Error::source(&self.kind)
    }
}

/// The `io::ErrorKind` is the kind's class: `UnexpectedEof` for truncation,
/// `OutOfMemory` for the memory limit, `Unsupported` for a check or filter
/// this build lacks, an LZMA2 failure's own class, and `InvalidData` for the
/// rest. The [`XzError`] is the payload, for `io::Error::downcast`.
impl From<XzError> for std::io::Error {
    fn from(e: XzError) -> Self {
        std::io::Error::new(e.kind.io_kind(), e)
    }
}

/// The result type the xz layer works in.
pub type XzResult<T> = Result<T, XzError>;
