//! Which match finder the encoder is using.
//!
//! C: `IMatchFinder2`, the vtable `MatchFinder_CreateVTable` or
//! `MatchFinderMt_CreateVTable` fills in and `LzmaEnc` then calls through
//! `p->matchFinder`. This is that indirection as an enum: two variants, chosen
//! once in `LzmaEnc_Alloc`, matched on at each call.
//!
//! An enum rather than a trait object because the single-threaded arm is the
//! hot one and must stay exactly as fast as it was: a `match` on a two-variant
//! enum whose discriminant does not change inside a loop is something the
//! compiler can hoist, and an indirect call is not.

use crate::enc::lz_find::MatchFinder;
use crate::enc::stream::SeqInStream;
use crate::error::Error;

#[cfg(feature = "std")]
use crate::enc::lz_find_mt::MatchFinderMt;

/// C: `p->matchFinderObj` together with the vtable that goes with it.
pub(crate) enum Finder {
    /// C: `MFB` with `MatchFinder_CreateVTable`.
    St(MatchFinder),
    /// C: `p->matchFinderMt` with `MatchFinderMt_CreateVTable`, used when
    /// `p->mtMode`.
    #[cfg(feature = "std")]
    Mt(MatchFinderMt),
}

impl Finder {
    /// C: `MatchFinder_Construct`.
    pub(crate) fn new() -> Self {
        Finder::St(MatchFinder::new())
    }

    /// Switches to the threaded finder, keeping the settings already applied.
    ///
    /// C: the `if (p->mtMode)` arm of `LzmaEnc_Alloc`, which picks the vtable.
    /// Doing it by moving the configured `CMatchFinder` across is this port's
    /// bookkeeping: the C has one `MFB` that both vtables read.
    #[cfg(feature = "std")]
    pub(crate) fn make_mt(&mut self) {
        if let Finder::St(mf) = self {
            let mut mt = MatchFinderMt::new();
            mt.mfb = core::mem::replace(mf, MatchFinder::new());
            *self = Finder::Mt(mt);
        }
    }

    /// Switches back to the single-threaded finder.
    #[cfg(feature = "std")]
    pub(crate) fn make_st(&mut self) {
        if let Finder::Mt(mt) = self {
            let mfb = core::mem::replace(&mut mt.mfb, MatchFinder::new());
            *self = Finder::St(mfb);
        }
    }

    /// Whether this is the threaded finder.
    #[cfg(test)]
    pub(crate) fn is_mt(&self) -> bool {
        #[cfg(feature = "std")]
        {
            matches!(self, Finder::Mt(_))
        }
        #[cfg(not(feature = "std"))]
        {
            false
        }
    }

    /// The `CMatchFinder` that carries the settings, whichever finder is in
    /// use. C: `MFB`, which `LzmaEnc_SetProps` and `LzmaEnc_Alloc` write
    /// regardless of `mtMode`.
    pub(crate) fn cfg(&mut self) -> &mut MatchFinder {
        match self {
            Finder::St(mf) => mf,
            #[cfg(feature = "std")]
            Finder::Mt(mt) => &mut mt.mfb,
        }
    }

    /// C: `MatchFinder_Create` or `MatchFinderMt_Create`.
    pub(crate) fn create(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
    ) -> Result<(), Error> {
        match self {
            Finder::St(mf) => mf.create(
                history_size,
                keep_add_buffer_before,
                match_max_len,
                keep_add_buffer_after,
            ),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.create(
                history_size,
                keep_add_buffer_before,
                match_max_len,
                keep_add_buffer_after,
            ),
        }
    }

    /// What this configuration would allocate, without allocating it.
    ///
    /// `mt` is `p->mtMode` as `LzmaEnc_Alloc` will compute it, not
    /// [`Finder::is_mt`]: the estimate is taken before anything is allocated,
    /// when the finder is still the single-threaded one it was constructed as.
    pub(crate) fn mem_usage(
        &mut self,
        mt: bool,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
    ) -> Result<u64, Error> {
        #[cfg(feature = "std")]
        if mt {
            return MatchFinderMt::mem_usage(
                self.cfg(),
                history_size,
                keep_add_buffer_before,
                match_max_len,
                keep_add_buffer_after,
            );
        }
        #[cfg(not(feature = "std"))]
        let _ = mt;
        self.cfg().mem_usage(
            history_size,
            keep_add_buffer_before,
            match_max_len,
            keep_add_buffer_after,
        )
    }

    /// What this finder has allocated, in bytes.
    #[cfg(test)]
    pub(crate) fn allocated(&self) -> u64 {
        match self {
            Finder::St(mf) => mf.allocated(),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.allocated(),
        }
    }

    /// C: `IMatchFinder2::Init`.
    pub(crate) fn init(&mut self, stream: &mut dyn SeqInStream) -> Result<(), Error> {
        match self {
            Finder::St(mf) => {
                mf.init(stream);
                Ok(())
            }
            #[cfg(feature = "std")]
            Finder::Mt(mt) => {
                // C: "call MatchFinderMt_InitMt() before IMatchFinder::Init()".
                mt.init_mt()?;
                mt.init();
                Ok(())
            }
        }
    }

    /// C: `IMatchFinder2::GetMatches`.
    #[inline]
    pub(crate) fn get_matches(&mut self, stream: &mut dyn SeqInStream, d: &mut [u32]) -> usize {
        match self {
            Finder::St(mf) => mf.get_matches(stream, d),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.get_matches(d),
        }
    }

    /// C: `IMatchFinder2::Skip`.
    #[inline]
    pub(crate) fn skip(&mut self, stream: &mut dyn SeqInStream, num: u32) {
        match self {
            Finder::St(mf) => mf.skip(stream, num),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.skip(num),
        }
    }

    /// C: `IMatchFinder2::GetNumAvailableBytes`.
    #[inline]
    pub(crate) fn get_num_available_bytes(&mut self) -> u32 {
        match self {
            Finder::St(mf) => mf.get_num_available_bytes(),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.get_num_available_bytes(),
        }
    }

    /// C: `IMatchFinder2::GetPointerToCurrentPos`, as an index into
    /// [`Finder::window`].
    #[inline]
    pub(crate) fn cur(&self) -> usize {
        match self {
            Finder::St(mf) => mf.cur(),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.cur(),
        }
    }

    /// The window `cur` indexes.
    #[inline]
    pub(crate) fn window(&self) -> &[u8] {
        match self {
            Finder::St(mf) => &mf.buf_base,
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.window(),
        }
    }

    /// The handle the threaded finder's producer threads are started from.
    ///
    /// C: there is no analogue - the C's threads are created once in
    /// `MatchFinderMt_Create` and live until the encoder is destroyed. Here
    /// they are scoped to one block, so the caller that owns the input for
    /// that block is the one that starts them.
    #[cfg(feature = "std")]
    pub(crate) fn mt_handle(&self) -> Option<alloc::sync::Arc<crate::enc::lz_find_mt::MtShared>> {
        match self {
            Finder::St(_) => None,
            Finder::Mt(mt) => mt.shared_handle().cloned(),
        }
    }

    /// C: `p->matchFinderMt.failure_LZ_BT`, which `CheckErrors` reads only in
    /// `mtMode`; for this port also a panic one of the finder's threads
    /// caught.
    pub(crate) fn mt_failed(&self) -> bool {
        match self {
            Finder::St(_) => false,
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.failed(),
        }
    }

    /// C: `mf->result`, the read error the window buffering saw.
    pub(crate) fn result(&self) -> Result<(), Error> {
        match self {
            Finder::St(mf) => mf.result,
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.result(),
        }
    }
}
