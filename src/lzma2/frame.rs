//! The LZMA2 chunk-header state machine, shared by the decoder and the parser.
//!
//! C: the `ELzma2State` enumeration and `Lzma2Dec_UpdateState` in
//! `C/Lzma2Dec.c`. The reference keeps these fields inside `CLzma2Dec` and
//! lets `Lzma2Dec_Parse` drive the same struct as `Lzma2Dec_DecodeToDic`; the
//! port splits them out so that [`crate::lzma2::Lzma2Parser`], which walks
//! chunk headers without a dictionary, can run the identical state machine
//! rather than a second copy of it.

use crate::lzma::LzmaProps;

/// C: `LZMA2_CONTROL_COPY_RESET_DIC`.
pub(crate) const LZMA2_CONTROL_COPY_RESET_DIC: u8 = 1;
/// C: `LZMA2_LCLP_MAX`.
pub(crate) const LZMA2_LCLP_MAX: u8 = 4;

/// C: `LZMA2_IS_UNCOMPRESSED_STATE(p)`.
pub(crate) const fn is_uncompressed_state(control: u8) -> bool {
    (control & (1 << 7)) == 0
}

/// C: `LZMA2_DIC_SIZE_FROM_PROP(p)`.
pub(crate) const fn dic_size_from_prop(prop: u8) -> u32 {
    (2u32 | (prop as u32 & 1)) << (prop as u32 / 2 + 11)
}

/// C: `LZMA2_DIC_SIZE_FROM_PROP_FULL(p)` in `CPP/7zip/Compress/Lzma2Decoder.cpp`,
/// which is the same formula with the `prop == 40` special case folded in.
pub(crate) const fn dic_size_from_prop_full(prop: u8) -> u32 {
    if prop == 40 {
        0xFFFF_FFFF
    } else {
        dic_size_from_prop(prop)
    }
}

/// C: `ELzma2State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lzma2State {
    Control,
    Unpack0,
    Unpack1,
    Pack0,
    Pack1,
    Prop,
    Data,
    DataCont,
    Finished,
    Error,
}

/// The chunk-header half of `CLzma2Dec`: everything `Lzma2Dec_UpdateState`
/// touches except the LZMA decoder itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lzma2Frame {
    pub(crate) state: Lzma2State,
    pub(crate) control: u8,
    pub(crate) need_init_level: u8,
    /// C: `isExtraMode`. Set while a compressed chunk's payload is being
    /// walked, so that `Lzma2Dec_GetUnpackExtra` can report how much output
    /// the chunk still owes.
    pub(crate) is_extra_mode: bool,
    pub(crate) unpack_size: u32,
    pub(crate) pack_size: u32,
}

impl Lzma2Frame {
    /// C: the `Lzma2Dec_Init` assignments that are not `LzmaDec_Init`.
    pub(crate) const fn new() -> Self {
        Lzma2Frame {
            state: Lzma2State::Control,
            control: 0,
            need_init_level: 0xE0,
            is_extra_mode: false,
            unpack_size: 0,
            pack_size: 0,
        }
    }

    /// C: `Lzma2Dec_Init`, for the framing fields.
    pub(crate) fn init(&mut self) {
        self.state = Lzma2State::Control;
        self.need_init_level = 0xE0;
        self.is_extra_mode = false;
        self.unpack_size = 0;
    }

    /// C: `Lzma2Dec_UpdateState`. `prop` is the LZMA decoder's property block,
    /// which the `LZMA2_STATE_PROP` case writes through.
    pub(crate) fn update_state(&mut self, b: u8, prop: &mut LzmaProps) -> Lzma2State {
        match self.state {
            Lzma2State::Control => {
                self.is_extra_mode = false;
                self.control = b;
                if b == 0 {
                    return Lzma2State::Finished;
                }
                if is_uncompressed_state(b) {
                    if b == LZMA2_CONTROL_COPY_RESET_DIC {
                        self.need_init_level = 0xC0;
                    } else if b > 2 || self.need_init_level == 0xE0 {
                        return Lzma2State::Error;
                    }
                } else {
                    if b < self.need_init_level {
                        return Lzma2State::Error;
                    }
                    self.need_init_level = 0;
                    self.unpack_size = u32::from(b & 0x1F) << 16;
                }
                Lzma2State::Unpack0
            }

            Lzma2State::Unpack0 => {
                self.unpack_size |= u32::from(b) << 8;
                Lzma2State::Unpack1
            }

            Lzma2State::Unpack1 => {
                self.unpack_size |= u32::from(b);
                self.unpack_size += 1;
                if is_uncompressed_state(self.control) {
                    Lzma2State::Data
                } else {
                    Lzma2State::Pack0
                }
            }

            Lzma2State::Pack0 => {
                self.pack_size = u32::from(b) << 8;
                Lzma2State::Pack1
            }

            Lzma2State::Pack1 => {
                self.pack_size |= u32::from(b);
                self.pack_size += 1;
                if self.control & 0x40 != 0 {
                    Lzma2State::Prop
                } else {
                    Lzma2State::Data
                }
            }

            Lzma2State::Prop => {
                let mut b = b;
                if b >= (9 * 5 * 5) {
                    return Lzma2State::Error;
                }
                let lc = b % 9;
                b /= 9;
                let pb = b / 5;
                let lp = b % 5;
                if lc + lp > LZMA2_LCLP_MAX {
                    return Lzma2State::Error;
                }
                prop.set_lclppb(lc, lp, pb);
                Lzma2State::Data
            }

            _ => Lzma2State::Error,
        }
    }
}
