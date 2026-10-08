//! Filter flags, the chains they may form, and the decode side of the chain.
//!
//! Spec §3.1.5 for how a filter is stored, §5.1-§5.3 for which filters exist
//! and how they may be chained. C: `XzDec.c`'s `XzBlock_Parse` and the
//! `CXzBlock.filters` array.
//!
//! A chain is at most four filters. The last one turns compressed bytes into
//! bytes (for this crate that is always LZMA2); the others are size-preserving
//! in-place converters — delta and the BCJ family — which the decoder applies
//! to the LZMA2 output in reverse order of the header's list.

use alloc::vec::Vec;

use super::bcj::{Bcj, BcjKind};
use super::delta::Delta;
use super::error::XzErrorKind;
use super::vli;

/// The LZMA2 filter id. Spec §5.3.1.
pub const FILTER_LZMA2: u64 = 0x21;
/// The delta filter id. Spec §5.3.3.
pub const FILTER_DELTA: u64 = 0x03;
/// The most filters a chain may have. Spec §5.2.
pub const MAX_FILTERS: usize = 4;
/// The most property bytes any filter this crate supports has.
const MAX_PROPS: usize = 4;

/// One filter as a block header stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterFlags {
    /// The filter id.
    pub id: u64,
    /// The filter's properties, at most four bytes for anything supported.
    pub props: [u8; MAX_PROPS],
    /// How many of `props` are real.
    pub props_len: usize,
}

impl FilterFlags {
    /// The property bytes.
    #[must_use]
    pub fn props(&self) -> &[u8] {
        &self.props[..self.props_len]
    }

    /// A filter-flags field from an id and its property bytes, for a caller
    /// building a chain for the writer rather than reading one.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::BadFilterChain`] if there are more property bytes than
    /// any filter this crate supports has.
    pub fn new(id: u64, props: &[u8]) -> Result<Self, XzErrorKind> {
        if props.len() > MAX_PROPS {
            return Err(XzErrorKind::BadFilterChain);
        }
        let mut buf = [0u8; MAX_PROPS];
        buf[..props.len()].copy_from_slice(props);
        Ok(FilterFlags {
            id,
            props: buf,
            props_len: props.len(),
        })
    }

    /// Parses one filter-flags field at `*pos`, advancing it.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::UnsupportedFilter`] for an id this crate does not
    /// implement, [`XzErrorKind::BadFilterChain`] for a properties field of
    /// the wrong size, and the VLI errors for a malformed integer.
    pub fn parse(buf: &[u8], pos: &mut usize) -> Result<Self, XzErrorKind> {
        let id = vli::decode_at(buf, pos)?;
        let size = vli::decode_at(buf, pos)?;
        // Every filter this crate supports has at most four property bytes;
        // an id it does not support is refused before the size is trusted,
        // so `size` is never used to index anything large.
        let want_props = match id {
            FILTER_LZMA2 => 1usize,
            FILTER_DELTA => 1,
            _ if BcjKind::from_filter_id(id).is_some() => {
                if size != 0 && size != 4 {
                    return Err(XzErrorKind::BadFilterChain);
                }
                size as usize
            }
            _ => return Err(XzErrorKind::UnsupportedFilter),
        };
        if size != want_props as u64 {
            return Err(XzErrorKind::BadFilterChain);
        }
        let mut props = [0u8; MAX_PROPS];
        let end = *pos + want_props;
        let slice = buf.get(*pos..end).ok_or(XzErrorKind::TruncatedInput)?;
        props[..want_props].copy_from_slice(slice);
        *pos = end;
        Ok(FilterFlags {
            id,
            props,
            props_len: want_props,
        })
    }

    /// The start offset a BCJ filter's four-byte property carries, or zero.
    fn start_offset(&self) -> u32 {
        if self.props_len == 4 {
            u32::from_le_bytes([self.props[0], self.props[1], self.props[2], self.props[3]])
        } else {
            0
        }
    }
}

/// A validated chain: the LZMA2 dictionary property plus the converters to
/// undo, in the order the decoder must undo them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterChain {
    /// The dictionary-size property byte of the chain's LZMA2 filter.
    pub dict_prop: u8,
    /// The non-last filters, in *decode* order: the header's list reversed,
    /// since filter 0 is the one the encoder applied first.
    pub converters: Vec<FilterFlags>,
}

impl FilterChain {
    /// Validates a header's filter list and works out the decode order.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::BadFilterChain`] if there is not exactly one last-filter
    /// (LZMA2) and it is not last, if a non-last filter appears last, if there
    /// are more than four, or if a BCJ start offset is misaligned. Spec §5.2
    /// and §5.3.
    ///
    /// A filter may appear more than once: `lzma_validate_chain` in liblzma's
    /// `filter_common.c` asks only for 1-4 filters, every non-last one to be
    /// allowed as non-last, the last one to be allowed as last, and at most
    /// three size-changing filters - and delta and the BCJ family change no
    /// sizes, so a chain of three deltas before LZMA2 is one xz decodes.
    pub fn validate(filters: &[FilterFlags]) -> Result<Self, XzErrorKind> {
        if filters.is_empty() || filters.len() > MAX_FILTERS {
            return Err(XzErrorKind::BadFilterChain);
        }
        let (last, rest) = filters.split_last().expect("non-empty");
        if last.id != FILTER_LZMA2 {
            return Err(XzErrorKind::BadFilterChain);
        }
        if last.props[0] & 0xC0 != 0 || last.props[0] > 40 {
            // Spec §5.3.1: bits 6-7 are reserved, and raw values over 40 are
            // dictionaries over 4 GiB.
            return Err(XzErrorKind::BadFilterChain);
        }
        for f in rest {
            if f.id == FILTER_LZMA2 {
                // A last-only filter used as a non-last one.
                return Err(XzErrorKind::BadFilterChain);
            }
            if let Some(kind) = BcjKind::from_filter_id(f.id) {
                if f.start_offset() % kind.alignment() != 0 {
                    return Err(XzErrorKind::BadFilterChain);
                }
            } else if f.id != FILTER_DELTA {
                return Err(XzErrorKind::UnsupportedFilter);
            }
        }
        let mut converters: Vec<FilterFlags> = rest.to_vec();
        converters.reverse();
        Ok(FilterChain {
            dict_prop: last.props[0],
            converters,
        })
    }

    /// Whether the chain is a bare LZMA2 filter, the common case, which the
    /// decoder can hand straight through with no copying.
    #[must_use]
    pub fn is_plain_lzma2(&self) -> bool {
        self.converters.is_empty()
    }

    /// Builds the runtime converters for one block.
    ///
    /// # Errors
    ///
    /// As [`FilterChain::validate`], for the properties it re-reads.
    pub fn build(&self) -> Result<Converters, XzErrorKind> {
        let mut stages = Vec::with_capacity(self.converters.len());
        for f in &self.converters {
            let stage = if f.id == FILTER_DELTA {
                Stage {
                    conv: Conv::Delta(Delta::new(f.props[0])?),
                    carry: Vec::new(),
                }
            } else {
                let kind = BcjKind::from_filter_id(f.id).ok_or(XzErrorKind::UnsupportedFilter)?;
                Stage {
                    conv: Conv::Bcj(Bcj::new(kind, f.start_offset())?),
                    carry: Vec::new(),
                }
            };
            stages.push(stage);
        }
        Ok(Converters {
            stages,
            scratch: Vec::new(),
        })
    }
}

#[derive(Debug)]
// The delta filter's 256-byte history is the whole of its state and is read
// and written by its inner loop, so it stays inline rather than behind a box.
#[allow(clippy::large_enum_variant)]
enum Conv {
    Delta(Delta),
    Bcj(Bcj),
}

#[derive(Debug)]
struct Stage {
    conv: Conv,
    /// Bytes the converter could not process yet: less than the filter's
    /// alignment plus its lookahead, so this is never more than 19 bytes.
    carry: Vec<u8>,
}

/// The non-last filters of one block, applied to the LZMA2 output as it
/// arrives.
///
/// Every converter is size-preserving and in place, so this does not buffer
/// output: it converts a chunk, hands the converted prefix on to the next
/// stage, and keeps each stage's short unconverted tail for the next chunk.
#[derive(Debug)]
pub struct Converters {
    stages: Vec<Stage>,
    scratch: Vec<u8>,
}

impl Converters {
    /// Whether there is anything to do at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// The most bytes the chain can be holding back at any moment.
    #[must_use]
    pub fn max_carry(&self) -> usize {
        self.stages
            .iter()
            .map(|s| match &s.conv {
                Conv::Delta(_) => 0,
                Conv::Bcj(b) => b.kind().max_carry(),
            })
            .sum()
    }

    /// Applies the whole chain to a whole block, in place.
    ///
    /// This is the shape a parallel decode has: the worker holds the block's
    /// entire output, so there is no chunking, no carry and no copy - each
    /// converter walks the buffer once, which is what the C does in
    /// `XzUnpacker_Code` when it is handed a complete block. The converters
    /// must be fresh; a chain that has already been pushed through holds a
    /// tail that this would ignore.
    ///
    /// Whatever a converter cannot process at the very end of the block - the
    /// last few bytes of an instruction that would straddle the end - stays as
    /// it is, which is what the encoder did with it too.
    pub fn apply_in_place(&mut self, data: &mut [u8]) {
        debug_assert!(self.stages.iter().all(|s| s.carry.is_empty()));
        for stage in &mut self.stages {
            match &mut stage.conv {
                Conv::Delta(d) => d.decode(data),
                Conv::Bcj(b) => {
                    b.decode(data);
                }
            }
        }
    }

    /// The same, in the other direction.
    ///
    /// The decoder undoes the converters in `stages` order; an encoder must
    /// apply them in the opposite one, because `stages` is the header's list
    /// reversed and the header lists the filter the encoder ran first.
    pub fn encode_in_place(&mut self, data: &mut [u8]) {
        debug_assert!(self.stages.iter().all(|s| s.carry.is_empty()));
        for stage in self.stages.iter_mut().rev() {
            match &mut stage.conv {
                Conv::Delta(d) => d.encode(data),
                Conv::Bcj(b) => {
                    b.encode(data);
                }
            }
        }
    }

    /// Pushes the next decoded bytes through every stage and appends whatever
    /// comes out the far end to `out`.
    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.run(data, out, false, false);
    }

    /// Pushes the last bytes of the block and flushes every stage's tail.
    ///
    /// At the end of a block there is nothing more to see, so a converter's
    /// unconverted tail is final and passes through as it stands — which is
    /// exactly what the C's loop does when `Load_more_input_data` returns
    /// nothing.
    pub fn finish(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.run(data, out, true, false);
    }

    /// Pushes the next bytes of a block being *written* through every stage,
    /// in encode order, and appends whatever comes out to `out`.
    ///
    /// Chunked or whole, the result is what [`Converters::encode_in_place`]
    /// makes of the same block: each converter keeps the tail it could not
    /// convert yet and converts it once the next chunk has arrived, exactly
    /// as [`Converters::push`] does in the other direction.
    pub fn encode_push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.run(data, out, false, true);
    }

    /// Pushes the last bytes of a block being written and flushes every
    /// stage's tail, which the end of the block leaves as it stands.
    pub fn encode_finish(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.run(data, out, true, true);
    }

    fn run(&mut self, data: &[u8], out: &mut Vec<u8>, flush: bool, encode: bool) {
        // `cur` walks down the stages; `scratch` is the other half of a
        // double buffer so no stage allocates per call.
        let mut cur = core::mem::take(&mut self.scratch);
        cur.clear();
        cur.extend_from_slice(data);

        let mut next = Vec::new();
        let stages = self.stages.len();
        for k in 0..stages {
            // `stages` is decode order; an encoder runs it backwards, as
            // `encode_in_place` does.
            let stage = &mut self.stages[if encode { stages - 1 - k } else { k }];
            let mut buf = core::mem::take(&mut stage.carry);
            buf.extend_from_slice(&cur);
            let done = match &mut stage.conv {
                Conv::Delta(d) => {
                    if encode {
                        d.encode(&mut buf);
                    } else {
                        d.decode(&mut buf);
                    }
                    buf.len()
                }
                Conv::Bcj(b) => {
                    let n = if encode {
                        b.encode(&mut buf)
                    } else {
                        b.decode(&mut buf)
                    };
                    if flush { buf.len() } else { n }
                }
            };
            next.clear();
            next.extend_from_slice(&buf[..done]);
            buf.drain(..done);
            stage.carry = buf;
            core::mem::swap(&mut cur, &mut next);
        }
        out.extend_from_slice(&cur);
        cur.clear();
        self.scratch = cur;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(id: u64, props: &[u8]) -> FilterFlags {
        let mut p = [0u8; MAX_PROPS];
        p[..props.len()].copy_from_slice(props);
        FilterFlags {
            id,
            props: p,
            props_len: props.len(),
        }
    }

    #[test]
    fn a_chain_must_end_in_lzma2_and_may_repeat_a_filter() {
        assert!(FilterChain::validate(&[flags(FILTER_LZMA2, &[20])]).is_ok());
        assert!(FilterChain::validate(&[flags(0x04, &[]), flags(FILTER_LZMA2, &[20])]).is_ok());
        // LZMA2 first is a last-filter used as a non-last one.
        assert_eq!(
            FilterChain::validate(&[flags(FILTER_LZMA2, &[20]), flags(0x04, &[])]),
            Err(XzErrorKind::BadFilterChain)
        );
        // No last filter at all.
        assert_eq!(
            FilterChain::validate(&[flags(0x04, &[])]),
            Err(XzErrorKind::BadFilterChain)
        );
        // The same converter twice, which the format allows: good-1-3delta
        // -lzma2.xz in XZ Utils' own test files stacks three deltas.
        assert!(
            FilterChain::validate(&[
                flags(0x04, &[]),
                flags(0x04, &[]),
                flags(FILTER_LZMA2, &[20])
            ])
            .is_ok()
        );
        assert!(
            FilterChain::validate(&[
                flags(FILTER_DELTA, &[0]),
                flags(FILTER_DELTA, &[1]),
                flags(FILTER_DELTA, &[2]),
                flags(FILTER_LZMA2, &[20])
            ])
            .is_ok()
        );
        // Five filters.
        assert_eq!(
            FilterChain::validate(&[
                flags(0x03, &[0]),
                flags(0x04, &[]),
                flags(0x05, &[]),
                flags(0x07, &[]),
                flags(FILTER_LZMA2, &[20])
            ]),
            Err(XzErrorKind::BadFilterChain)
        );
        // A dictionary property out of range.
        assert_eq!(
            FilterChain::validate(&[flags(FILTER_LZMA2, &[41])]),
            Err(XzErrorKind::BadFilterChain)
        );
        assert_eq!(
            FilterChain::validate(&[flags(FILTER_LZMA2, &[0xC0])]),
            Err(XzErrorKind::BadFilterChain)
        );
    }

    #[test]
    fn the_decode_order_is_the_headers_order_reversed() {
        let chain = FilterChain::validate(&[
            flags(0x03, &[3]),
            flags(0x04, &[]),
            flags(FILTER_LZMA2, &[20]),
        ])
        .expect("valid");
        assert_eq!(chain.dict_prop, 20);
        assert_eq!(chain.converters[0].id, 0x04);
        assert_eq!(chain.converters[1].id, FILTER_DELTA);
    }

    #[test]
    fn filter_flags_reject_the_wrong_property_size() {
        let mut pos = 0;
        // LZMA2 with two property bytes.
        assert_eq!(
            FilterFlags::parse(&[0x21, 0x02, 0x00, 0x00], &mut pos),
            Err(XzErrorKind::BadFilterChain)
        );
        let mut pos = 0;
        // A BCJ filter with three.
        assert_eq!(
            FilterFlags::parse(&[0x04, 0x03, 0, 0, 0], &mut pos),
            Err(XzErrorKind::BadFilterChain)
        );
        let mut pos = 0;
        // An id the format defines no filter for.
        assert_eq!(
            FilterFlags::parse(&[0x19, 0x00], &mut pos),
            Err(XzErrorKind::UnsupportedFilter)
        );
    }
}
