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

/// Which finder `LzmaEnc_Alloc` picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinderMode {
    /// C: `MatchFinder_CreateVTable`, a hash chain, or a fast-mode tree.
    St,
    /// C: `MatchFinderMt_CreateVTable`, `p->mtMode`.
    Threaded,
    /// The threaded finder's stages run on the caller's thread a block at a
    /// time: a binary tree in normal mode held to one thread, where the C
    /// takes the single-threaded finder. The matches are the same; see
    /// `crate::enc::lz_find_mt::MatchFinderMt::get_next_block_bt`.
    Inline,
}

/// C: `p->matchFinderObj` together with the vtable that goes with it.
pub(crate) enum Finder {
    /// C: `MFB` with `MatchFinder_CreateVTable`.
    St(MatchFinder),
    /// C: `p->matchFinderMt` with `MatchFinderMt_CreateVTable`, used when
    /// `p->mtMode`, and with no threads in [`FinderMode::Inline`].
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

    /// Whether this is the threaded finder, with its threads.
    #[cfg(test)]
    pub(crate) fn is_mt(&self) -> bool {
        #[cfg(feature = "std")]
        {
            matches!(self, Finder::Mt(mt) if !mt.is_inline())
        }
        #[cfg(not(feature = "std"))]
        {
            false
        }
    }

    /// Whether this is the threaded finder run on the caller's thread.
    #[cfg(test)]
    pub(crate) fn is_inline(&self) -> bool {
        #[cfg(feature = "std")]
        {
            matches!(self, Finder::Mt(mt) if mt.is_inline())
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

    /// C: `MatchFinder_Create` or `MatchFinderMt_Create`. `data_limit` is the
    /// promise [`MatchFinder::create`] documents. `direct` asks the threaded
    /// finder for no window, the input being a block it will read in place
    /// (`MatchFinderMt::create`); the single-threaded finder always reads
    /// through its window and ignores it. `inline` is
    /// [`FinderMode::Inline`], for the threaded finder.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        data_limit: u64,
        #[cfg_attr(not(feature = "std"), allow(unused_variables))] direct: bool,
        #[cfg_attr(not(feature = "std"), allow(unused_variables))] inline: bool,
    ) -> Result<(), Error> {
        match self {
            Finder::St(mf) => {
                // A finder switched back from the threaded one keeps the
                // flag that one was last created with.
                mf.direct_input = false;
                mf.create(
                    history_size,
                    keep_add_buffer_before,
                    match_max_len,
                    keep_add_buffer_after,
                    data_limit,
                )
            }
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.create(
                history_size,
                keep_add_buffer_before,
                match_max_len,
                keep_add_buffer_after,
                data_limit,
                direct,
                inline,
            ),
        }
    }

    /// What this configuration would allocate, without allocating it.
    ///
    /// `mode` is the finder `LzmaEnc_Alloc` will pick, not the one in use:
    /// the estimate is taken before anything is allocated, when the finder is
    /// still the single-threaded one it was constructed as. `direct` is
    /// [`Finder::create`]'s: the threaded finder, inline or not, then counts
    /// no window, and the single-threaded one, which always has one, ignores
    /// it.
    pub(crate) fn mem_usage(
        &mut self,
        mode: FinderMode,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        #[cfg_attr(not(feature = "std"), allow(unused_variables))] direct: bool,
    ) -> Result<u64, Error> {
        #[cfg(feature = "std")]
        if mode != FinderMode::St {
            return MatchFinderMt::mem_usage(
                self.cfg(),
                history_size,
                keep_add_buffer_before,
                match_max_len,
                keep_add_buffer_after,
                direct,
                mode == FinderMode::Inline,
            );
        }
        #[cfg(not(feature = "std"))]
        let _ = mode;
        self.cfg().mem_usage(
            history_size,
            keep_add_buffer_before,
            match_max_len,
            keep_add_buffer_after,
            true,
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
            Finder::Mt(mt) => mt.skip(stream, num),
        }
    }

    /// C: `IMatchFinder2::GetNumAvailableBytes`. `stream` is read only by
    /// the inline finder, which fills its next block here.
    #[inline]
    pub(crate) fn get_num_available_bytes(
        &mut self,
        #[cfg_attr(not(feature = "std"), allow(unused_variables))] stream: &mut dyn SeqInStream,
    ) -> u32 {
        match self {
            Finder::St(mf) => mf.get_num_available_bytes(),
            #[cfg(feature = "std")]
            Finder::Mt(mt) => mt.get_num_available_bytes(stream),
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

    /// The handle a stream's pair of producer threads is started from, by
    /// [`crate::enc::lz_find_mt::with_threads`].
    ///
    /// C: there is no analogue - the C's threads are created once in
    /// `MatchFinderMt_Create` and live until the encoder is destroyed. On the
    /// stream path they are scoped to the stream, so the caller that owns the
    /// input is the one that starts them.
    #[cfg(feature = "std")]
    pub(crate) fn mt_handle(
        &mut self,
    ) -> Option<alloc::sync::Arc<crate::enc::lz_find_mt::MtShared>> {
        match self {
            Finder::St(_) => None,
            Finder::Mt(mt) => mt.stream_handle(),
        }
    }

    /// The handle [`crate::enc::lz_find_mt::run_block`] runs a block with,
    /// the finder's pair of producer threads running and kept for the next.
    ///
    /// # Errors
    ///
    /// [`Error::Alloc`] if a thread cannot be started.
    #[cfg(feature = "std")]
    pub(crate) fn mt_block_handle(
        &mut self,
    ) -> Result<Option<alloc::sync::Arc<crate::enc::lz_find_mt::MtShared>>, Error> {
        match self {
            Finder::St(_) => Ok(None),
            Finder::Mt(mt) => mt.block_handle(),
        }
    }

    /// How many pairs of producer threads the finder has started for blocks.
    #[cfg(all(test, feature = "std"))]
    pub(crate) fn mt_spawns(&self) -> u32 {
        match self {
            Finder::St(_) => 0,
            Finder::Mt(mt) => mt.spawns,
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
