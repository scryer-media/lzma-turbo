//! Encoder constants.
//!
//! C: the `#define` block at the top of `C/LzmaEnc.c`, plus `C/LzHash.h`.
//! Names are the C names in `SCREAMING_SNAKE_CASE`. Where the encoder and the
//! decoder name the same number, the decoder's definition in
//! [`crate::lzma::consts`] is used rather than a second copy.

pub(crate) use crate::lzma::consts::{
    K_ALIGN_TABLE_SIZE, K_BIT_MODEL_TOTAL, K_END_POS_MODEL_INDEX, K_LEN_NUM_HIGH_BITS,
    K_LEN_NUM_HIGH_SYMBOLS, K_LEN_NUM_LOW_BITS, K_LEN_NUM_LOW_SYMBOLS, K_NUM_ALIGN_BITS,
    K_NUM_BIT_MODEL_TOTAL_BITS, K_NUM_FULL_DISTANCES, K_NUM_LEN_TO_POS_STATES, K_NUM_MOVE_BITS,
    K_NUM_POS_SLOT_BITS, K_NUM_STATES, K_START_POS_MODEL_INDEX, K_TOP_VALUE, LZMA_PROPS_SIZE,
};

/// C: `kLzmaMaxHistorySize`. "for good normalization speed we still reserve
/// 256 MB before 4 GB range".
pub(crate) const K_LZMA_MAX_HISTORY_SIZE: u32 = 15 << 28;

/// C: `kProbInitValue`.
pub(crate) const K_PROB_INIT_VALUE: u16 = (K_BIT_MODEL_TOTAL >> 1) as u16;

/// C: `kNumMoveReducingBits`.
pub(crate) const K_NUM_MOVE_REDUCING_BITS: u32 = 4;
/// C: `kNumBitPriceShiftBits`.
pub(crate) const K_NUM_BIT_PRICE_SHIFT_BITS: u32 = 4;

/// C: `REP_LEN_COUNT`.
pub(crate) const REP_LEN_COUNT: i32 = 64;

/// C: `kNumLogBits`, for the 64-bit build (`11 + sizeof(size_t) / 8 * 3`).
/// `g_FastPos` is sized from this constant and the slot it returns must not
/// depend on the host, so the 64-bit value is used on every target that
/// builds the table. That costs 2 KiB of table on a 32-bit target and makes
/// the output identical on both. ARM and ARM64 do not build it: like the C
/// there, they take the slot from a leading-zero count, and only the tests
/// build the table on them, to check the two agree.
#[cfg(any(
    all(test, feature = "std"),
    not(any(target_arch = "arm", target_arch = "aarch64"))
))]
pub(crate) const K_NUM_LOG_BITS: usize = 11 + 3;

/// C: `LZMA_NUM_REPS`.
pub(crate) const LZMA_NUM_REPS: usize = 4;

/// C: `kNumOpts`.
pub(crate) const K_NUM_OPTS: usize = 1 << 11;
/// C: `kPackReserve`.
pub(crate) const K_PACK_RESERVE: usize = K_NUM_OPTS * 8;

/// C: `kDicLogSizeMax`.
pub(crate) const K_DIC_LOG_SIZE_MAX: u32 = 32;
/// C: `kDistTableSizeMax`.
pub(crate) const K_DIST_TABLE_SIZE_MAX: usize = (K_DIC_LOG_SIZE_MAX * 2) as usize;

/// C: `kAlignMask`.
pub(crate) const K_ALIGN_MASK: u32 = K_ALIGN_TABLE_SIZE as u32 - 1;

/// C: `LZMA_PB_MAX`.
pub(crate) const LZMA_PB_MAX: u32 = 4;
/// C: `LZMA_LC_MAX`.
pub(crate) const LZMA_LC_MAX: u32 = 8;
/// C: `LZMA_LP_MAX`.
pub(crate) const LZMA_LP_MAX: u32 = 4;

/// C: `LZMA_NUM_PB_STATES_MAX`.
pub(crate) const LZMA_NUM_PB_STATES_MAX: usize = 1 << LZMA_PB_MAX;

/// C: `kLenNumSymbolsTotal`.
pub(crate) const K_LEN_NUM_SYMBOLS_TOTAL: usize =
    (K_LEN_NUM_LOW_SYMBOLS * 2 + K_LEN_NUM_HIGH_SYMBOLS) as usize;

/// C: `LZMA_MATCH_LEN_MIN`.
pub const LZMA_MATCH_LEN_MIN: u32 = 2;
/// C: `LZMA_MATCH_LEN_MAX`.
pub const LZMA_MATCH_LEN_MAX: u32 = LZMA_MATCH_LEN_MIN + K_LEN_NUM_SYMBOLS_TOTAL as u32 - 1;

/// C: `kInfinityPrice`.
pub(crate) const K_INFINITY_PRICE: u32 = 1 << 30;

/// C: `MARK_LIT`. The `dist` value that marks an optimum entry as a literal.
pub(crate) const MARK_LIT: u32 = u32::MAX;

/// C: `kState_Start`.
pub(crate) const K_STATE_START: usize = 0;
/// C: `kState_LitAfterMatch`.
pub(crate) const K_STATE_LIT_AFTER_MATCH: u32 = 4;
/// C: `kState_LitAfterRep`.
pub(crate) const K_STATE_LIT_AFTER_REP: u32 = 5;
/// C: `kState_MatchAfterLit`.
pub(crate) const K_STATE_MATCH_AFTER_LIT: u32 = 7;
/// C: `kState_RepAfterLit`.
pub(crate) const K_STATE_REP_AFTER_LIT: u32 = 8;

/// C: `kLiteralNextStates`.
pub(crate) const K_LITERAL_NEXT_STATES: [u8; K_NUM_STATES as usize] =
    [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5];
/// C: `kMatchNextStates`.
pub(crate) const K_MATCH_NEXT_STATES: [u8; K_NUM_STATES as usize] =
    [7, 7, 7, 7, 7, 7, 7, 10, 10, 10, 10, 10];
/// C: `kRepNextStates`.
pub(crate) const K_REP_NEXT_STATES: [u8; K_NUM_STATES as usize] =
    [8, 8, 8, 8, 8, 8, 8, 11, 11, 11, 11, 11];
/// C: `kShortRepNextStates`.
pub(crate) const K_SHORT_REP_NEXT_STATES: [u8; K_NUM_STATES as usize] =
    [9, 9, 9, 9, 9, 9, 9, 11, 11, 11, 11, 11];

/// C: `IsLitState(s)`.
#[inline]
pub(crate) const fn is_lit_state(s: u32) -> bool {
    s < 7
}

/// C: `GetLenToPosState2(len)`.
#[inline]
pub(crate) const fn get_len_to_pos_state2(len: u32) -> usize {
    if (len as usize) < K_NUM_LEN_TO_POS_STATES - 1 {
        len as usize
    } else {
        K_NUM_LEN_TO_POS_STATES - 1
    }
}

/// C: `GetLenToPosState(len)`.
#[inline]
pub(crate) const fn get_len_to_pos_state(len: u32) -> usize {
    if (len as usize) < K_NUM_LEN_TO_POS_STATES + 1 {
        (len - 2) as usize
    } else {
        K_NUM_LEN_TO_POS_STATES - 1
    }
}

/// C: `RC_BUF_SIZE`.
pub(crate) const RC_BUF_SIZE: usize = 1 << 16;

/// C: `kBigHashDicLimit`.
pub(crate) const K_BIG_HASH_DIC_LIMIT: u32 = 1 << 24;

// ---------------------------------------------------------------------------
// C: `C/LzHash.h`
// ---------------------------------------------------------------------------

/// C: `kHash2Size`.
pub(crate) const K_HASH2_SIZE: u32 = 1 << 10;
/// C: `kHash3Size`.
pub(crate) const K_HASH3_SIZE: u32 = 1 << 16;
/// C: `kFix3HashSize`.
pub(crate) const K_FIX3_HASH_SIZE: usize = K_HASH2_SIZE as usize;
/// C: `kFix4HashSize`.
pub(crate) const K_FIX4_HASH_SIZE: usize = (K_HASH2_SIZE + K_HASH3_SIZE) as usize;
/// C: `kFix5HashSize`, which `LzFind.c` defines as `kFix4HashSize`.
pub(crate) const K_FIX5_HASH_SIZE: usize = K_FIX4_HASH_SIZE;
/// C: `kLzHash_CrcShift_1`.
pub(crate) const K_LZ_HASH_CRC_SHIFT_1: u32 = 5;
/// C: `kLzHash_CrcShift_2`.
pub(crate) const K_LZ_HASH_CRC_SHIFT_2: u32 = 10;

// ---------------------------------------------------------------------------
// C: `C/LzFind.c`
// ---------------------------------------------------------------------------

/// C: `kBlockMoveAlign`, the alignment `MatchFinder_MoveBlock` moves on.
pub(crate) const K_BLOCK_MOVE_ALIGN: usize = 1 << 7;
/// C: `kBlockSizeAlign`.
pub(crate) const K_BLOCK_SIZE_ALIGN: u32 = 1 << 16;
/// C: `kBlockSizeReserveMin`.
pub(crate) const K_BLOCK_SIZE_RESERVE_MIN: u32 = 1 << 24;
/// C: `kEmptyHashValue`.
pub(crate) const K_EMPTY_HASH_VALUE: u32 = 0;
/// C: `kMaxValForNormalize`.
pub(crate) const K_MAX_VAL_FOR_NORMALIZE: u32 = 0;
/// C: `NUM_REFS_ALIGN_MASK`.
pub(crate) const NUM_REFS_ALIGN_MASK: usize = 0xF;

const _: () = assert!(K_BLOCK_SIZE_RESERVE_MIN >= K_BLOCK_SIZE_ALIGN * 2);
const _: () = assert!(LZMA_MATCH_LEN_MAX == 273);

/// C: `LZMA2_LCLP_MAX` in `C/Lzma2Enc.c`.
pub(crate) const LZMA2_LCLP_MAX: i32 = 4;
