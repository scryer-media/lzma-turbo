//! The LZMA encoder.
//!
//! A faithful port of the encoder side of Igor Pavlov's LZMA SDK: `C/LzFind.c`
//! (the match finders), `C/LzmaEnc.c` (the range coder, the price tables and
//! the optimal parser) and `C/Lzma2Enc.c` (LZMA2 chunking). See
//! `docs/encoder.md` for what was left out and how parity is tested.

// The register-shaped tree walk: what the bt thread runs on x86-64 with
// `asm`, the other arm of `kernel-ab`, and under test everywhere.
#[cfg(all(
    feature = "std",
    any(
        test,
        feature = "kernel-ab",
        all(feature = "asm", target_arch = "x86_64")
    )
))]
mod bt_kernel;
mod consts;
mod finder;
mod huge_pages;
mod lz_find;
#[cfg(feature = "std")]
mod lz_find_mt;
mod lzma2_enc;
mod lzma_enc;
mod match_run;
#[cfg(feature = "std")]
mod mt_coder;
#[cfg(feature = "std")]
mod pipe;
mod price;
mod props;
mod push;
mod range_enc;
mod stream;
#[cfg(feature = "std")]
mod write;
#[cfg(feature = "xz")]
mod xz_enc;

use alloc::vec::Vec;

use crate::error::Error;
use lzma_enc::LzmaEnc;

pub use consts::{LZMA_MATCH_LEN_MAX, LZMA_MATCH_LEN_MIN};
pub use lz_find::MatchFinderKind;
pub use lzma2_enc::{
    BLOCK_SIZE_AUTO, BLOCK_SIZE_SOLID, Lzma2Encoder, auto_block_size, encode_lzma2, encode_lzma2_mt,
};
pub use props::{LzmaEncProps, NormalizedProps};
pub use push::{Lzma2PushEncoder, LzmaPushEncoder};
pub use stream::{SeqInStream, SeqOutStream, SliceStream};
#[cfg(feature = "xz")]
pub use write::XzWriter;
#[cfg(feature = "std")]
pub use write::{Lzma2Writer, LzmaWriter};
#[cfg(feature = "xz")]
pub use xz_enc::{DEFAULT_BLOCK_SIZE, XzEncoder, encode_xz, encode_xz_mt, encode_xz_with_filters};

/// An LZMA1 encoder.
///
/// C: `CLzmaEncHandle`, driven by `LzmaEnc_Encode`.
///
/// The encoder holds its match finder window and probability model, so reusing
/// one across streams is much cheaper than building a new one — but each
/// [`LzmaEncoder::encode`] call re-initializes both, exactly as the C's
/// `LzmaEnc_Prepare` does, so consecutive streams are independent.
pub struct LzmaEncoder {
    inner: alloc::boxed::Box<LzmaEnc>,
}

impl LzmaEncoder {
    /// C: `LzmaEnc_Create` followed by `LzmaEnc_SetProps`.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range, [`Error::Alloc`] if the
    /// encoder state could not be allocated.
    pub fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        let mut inner = alloc::boxed::Box::new(LzmaEnc::new()?);
        inner.set_props(props)?;
        Ok(LzmaEncoder { inner })
    }

    /// C: `LzmaEnc_SetProps` on an encoder that has been used: `props` in
    /// place of the settings it was built with. The next stream is written
    /// byte for byte as a new encoder with these settings would write it,
    /// and the match finder keeps its window and tables while they are large
    /// enough for them, as the C's `MatchFinder_Create` keeps them.
    ///
    /// # Errors
    ///
    /// As [`LzmaEncoder::new`], with the encoder left as it was.
    pub fn set_props(&mut self, props: &LzmaEncProps) -> Result<(), Error> {
        self.inner.set_props(props)
    }

    /// The five LZMA property bytes a decoder needs for this setting.
    ///
    /// C: `LzmaEnc_WriteProperties`.
    #[must_use]
    pub fn properties(&self) -> [u8; crate::lzma::consts::LZMA_PROPS_SIZE] {
        self.inner.write_properties()
    }

    /// The dictionary size this encoder will use.
    #[must_use]
    pub fn dict_size(&self) -> u32 {
        self.inner.dict_size
    }

    /// Encode `input` into `out`.
    ///
    /// C: `LzmaEnc_Encode`. Whether the stream ends with an end marker is the
    /// `write_end_mark` setting; without one the decoder needs the uncompressed
    /// size from elsewhere. The match finder runs on this thread whatever
    /// [`LzmaEncProps::with_num_threads`] says, because a stream that is not
    /// `Send` cannot be given to its threads; [`LzmaEncoder::encode_send`] is
    /// the one that starts them.
    ///
    /// # Errors
    ///
    /// Whatever the streams return, or [`Error::Alloc`].
    pub fn encode(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        // A stream that is not `Send` cannot be given to the threaded
        // finder's threads; see `LzmaEnc::finder_threads`.
        self.inner.finder_threads = false;
        let res = self.inner.prepare(0);
        self.inner.finder_threads = true;
        res?;
        self.encode_prepared(input, out)
    }

    /// Encode `input` into `out`, telling the encoder how long the input is.
    ///
    /// C: `LzmaEnc_SetDataSize` before `LzmaEnc_Encode`, which is what
    /// `LzmaEnc_MemEncode` does implicitly. The size changes the output: it
    /// sizes the match finder's hash table, so an encoder told the size can
    /// find different matches from one that was not.
    ///
    /// # Errors
    ///
    /// As [`LzmaEncoder::encode`].
    pub fn encode_sized(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
        input_len: u64,
    ) -> Result<(), Error> {
        self.inner.finder_threads = false;
        let res = self.inner.mem_prepare(input_len, 0);
        self.inner.finder_threads = true;
        res?;
        self.encode_prepared(input, out)
    }

    /// C: `LzmaEnc_Encode2`, without the progress callback.
    fn encode_prepared(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        self.block_loop(input, out)
    }

    /// C: the `for (;;) { LzmaEnc_CodeOneBlock(...) }` loop of
    /// `LzmaEnc_Encode2`.
    fn block_loop(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        loop {
            self.inner.code_one_block(input, out, 0, 0)?;
            if self.inner.finished {
                return Ok(());
            }
        }
    }

    /// As [`LzmaEncoder::encode_prepared`], for an input the threaded match
    /// finder's hash thread can be given. See
    /// [`LzmaEncProps::with_num_threads`].
    #[cfg(feature = "std")]
    fn encode_prepared_send(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        if let Some(sh) = self.inner.mt_handle() {
            return crate::enc::lz_find_mt::with_threads(&sh, input, || {
                self.block_loop(&mut crate::enc::stream::NoStream, out)
            });
        }
        self.block_loop(input, out)
    }

    /// Encode a stream that can be sent.
    ///
    /// Same bytes as [`LzmaEncoder::encode`]; the `Send` bound is what lets
    /// [`LzmaEncProps::with_num_threads`] start the match finder's threads.
    ///
    /// # Errors
    ///
    /// As [`LzmaEncoder::encode`].
    #[cfg(feature = "std")]
    pub fn encode_send(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        self.inner.prepare(0)?;
        self.encode_prepared_send(input, out)
    }

    /// Encode a stream that can be sent, telling the encoder how long it is.
    ///
    /// Same bytes as [`LzmaEncoder::encode_sized`]; the `Send` bound is what
    /// lets [`LzmaEncProps::with_num_threads`] start the match finder's
    /// threads.
    ///
    /// # Errors
    ///
    /// As [`LzmaEncoder::encode`].
    #[cfg(feature = "std")]
    pub fn encode_sized_send(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut dyn SeqOutStream,
        input_len: u64,
    ) -> Result<(), Error> {
        self.inner.mem_prepare(input_len, 0)?;
        self.encode_prepared_send(input, out)
    }

    /// Encode a slice, returning the raw LZMA1 stream.
    ///
    /// C: `LzmaEnc_MemEncode`, which sets the data size from `srcLen`.
    ///
    /// # Errors
    ///
    /// [`Error::Alloc`] if the output could not be grown.
    pub fn encode_to_vec(&mut self, src: &[u8]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut input = SliceStream::new(src);
        #[cfg(feature = "std")]
        self.encode_sized_send(&mut input, &mut out, src.len() as u64)?;
        #[cfg(not(feature = "std"))]
        self.encode_sized(&mut input, &mut out, src.len() as u64)?;
        Ok(out)
    }

    /// The number of input bytes encoded by the last call.
    #[must_use]
    pub fn processed_in(&self) -> u64 {
        self.inner.now_pos64
    }
}

/// Encode `src` as a `.lzma` (LZMA-Alone) file.
///
/// The 13-byte header is the five property bytes and the uncompressed size as
/// a little-endian 64-bit number, which is what
/// [`crate::LzmaAloneHeader::parse`] reads back.
///
/// # Errors
///
/// [`Error::Param`] if a setting is out of range, [`Error::Alloc`] on
/// allocation failure.
pub fn encode_lzma_alone(src: &[u8], props: &LzmaEncProps) -> Result<Vec<u8>, Error> {
    let mut enc = LzmaEncoder::new(props)?;
    let mut out = Vec::new();
    out.try_reserve(crate::lzma_alone::LZMA_ALONE_HEADER_SIZE)
        .map_err(|_| Error::Alloc)?;
    out.extend_from_slice(&enc.properties());
    out.extend_from_slice(&(src.len() as u64).to_le_bytes());
    let mut input = SliceStream::new(src);
    #[cfg(feature = "std")]
    enc.encode_sized_send(&mut input, &mut out, src.len() as u64)?;
    #[cfg(not(feature = "std"))]
    enc.encode_sized(&mut input, &mut out, src.len() as u64)?;
    Ok(out)
}
