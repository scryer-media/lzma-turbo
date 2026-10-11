//! The bt thread's tree walk in the shape of the SDK's x86-64 assembly.
//!
//! C: `GetMatchesSpecN_2` in `Asm/x86/LzFindOpt.asm` (SDK 26.03), the routine
//! the shipped 7-Zip runs on x86-64 in place of the C of the same name in
//! `C/LzFindOpt.c`. The two compute the same thing; the assembly differs in
//! where the walk's state lives. Every value the per-node loop carries from one
//! node to the next - the cursor, the distance, the two link pointers, the two
//! bounds and the best length - is in a register (`LzFindOpt.asm:50-81`), the
//! tree pointers are formed as `son + 8 * cycPos` (`:232`, `:285`) rather than
//! as indices to bounds-check, and only the cut counter and the cyclic size go
//! to the stack (`:136-147`, `:308-309`).
//!
//! [`super::lz_find_mt`]'s `get_matches_spec_n_2` computes the same walk with
//! slice indices into the window and the tree, and the compiler inlines it into
//! the bt thread's loop. There, on x86-64, the node's link value is spilled to
//! the stack and read back to form the next node's distance, so a
//! store-forwarding delay sits on the chain of dependent loads the walk is made
//! of, and a dozen more stack accesses fill the rest of the node. This one is
//! kept out of line (`#[inline(never)]`, as the assembly is a call), works from
//! a cursor pointer and offsets from it as the assembly does (`cur`, `len`,
//! `lenLimit` at `:196-203`), and reads the tree through one base pointer.
//!
//! What is the same as the slice version, and is tested to be: the five
//! conditions it asserts at entry, the answer, every write to the tree and to
//! `d`, and every refusal. The match-extension scan is the crate's eight-byte
//! one (`match_run`), not the assembly's byte loop (`:376-384`), because that
//! is what the encode measured faster; it is written here over the cursor
//! pointer so the scan's limits are the walk's own.
//!
//! The walk's bounds argument is the slice version's, word for word; see
//! "# Bounds" on `get_matches_spec_n_2`. The accesses made through raw
//! pointers here are exactly the ones that version leaves unchecked, plus the
//! empty-node and long-match writes to the tree, which it checks and which this
//! checks with an explicit test against the tree's length: once the tree's base
//! pointer exists, every access to the tree has to go through it.

use crate::enc::consts::K_EMPTY_HASH_VALUE;

/// The first offset in `from ..= lim` from `cur` at which the byte there and
/// the byte `diff` before it differ, or `lim` if none does.
///
/// # Safety
///
/// `cur + lim` is at most the window's length and `diff` is at most
/// `cur + from` (both as offsets into the window `cur` points into): every
/// byte read is in `cur + from - diff .. cur + lim`.
#[inline(always)]
unsafe fn scan(cur: *const u8, diff: usize, mut len: usize, lim: usize) -> usize {
    while len + 8 <= lim {
        // SAFETY: `len + 8 <= lim`, and both words start at or after
        // `from - diff` relative to `cur` - the caller's contract.
        let (a, b) = unsafe {
            (
                u64::from_le(cur.add(len).sub(diff).cast::<u64>().read_unaligned()),
                u64::from_le(cur.add(len).cast::<u64>().read_unaligned()),
            )
        };
        let x = a ^ b;
        if x != 0 {
            // Little-endian words: the lowest set bit is the first byte in
            // address order that differs.
            return len + (x.trailing_zeros() as usize >> 3);
        }
        len += 8;
    }
    // SAFETY: `len < lim`, the caller's contract as above.
    while len < lim && unsafe { *cur.add(len).sub(diff) == *cur.add(len) } {
        len += 1;
    }
    len
}

/// C: `GetMatchesSpecN_2` (`Asm/x86/LzFindOpt.asm`, `C/LzFindOpt.c`).
///
/// The same arguments, answer and effects as `get_matches_spec_n_2` in
/// [`super::lz_find_mt`], which documents them; see the module comment for how
/// it differs.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
#[inline(never)]
pub(crate) fn get_matches_spec_n_2_reg(
    win: &[u8],
    len_limit_0: usize,
    mut pos: u32,
    mut cur: usize,
    son: &mut [u32],
    cut_value: u32,
    d: &mut [u32],
    mut di: usize,
    max_len_0: usize,
    heads: &[u32],
    mut hi: usize,
    limit: usize,
    hsize: usize,
    mut cyclic_buffer_pos: u32,
    cyclic_buffer_size: u32,
) -> Option<(usize, u32)> {
    if hi == hsize {
        return Some((di, pos));
    }
    // The five conditions of "# Bounds", tested as the slice version tests
    // them, with the same messages.
    assert!(hi < hsize && hsize <= heads.len(), "heads");
    let n = hsize - hi;
    let cyclic = cyclic_buffer_size as usize;
    assert!(
        (cyclic_buffer_pos as usize)
            .checked_add(n)
            .is_some_and(|end| end <= cyclic),
        "A: the run passes the end of the cyclic buffer"
    );
    assert!(son.len() / 2 >= cyclic, "B: the tree is short");
    assert!(
        cur.checked_add(max_len_0)
            .is_some_and(|end| end <= len_limit_0),
        "C: no room for the hashed bytes below the limit"
    );
    assert!(
        len_limit_0
            .checked_add(n)
            .is_some_and(|end| end <= win.len()),
        "D: the run passes the end of the window"
    );
    assert!(
        pos.min(cyclic_buffer_size) as usize <= cur + 1,
        "E: a distance could reach before the window"
    );

    // From here on every access to the tree goes through `tree`, so that no
    // reborrow of `son` ends the pointer's validity while it is in use.
    let son_len = son.len();
    let tree = son.as_mut_ptr();
    let base = win.as_ptr();

    let mut len_limit = len_limit_0;
    loop {
        if hi == hsize {
            break;
        }
        let mut delta = heads[hi];
        hi += 1;
        if delta == 0 {
            return None;
        }
        len_limit += 1;

        let mut cbs = cyclic_buffer_size;
        if pos < cbs {
            if delta > pos {
                return None;
            }
            cbs = pos;
        }

        if delta >= cbs {
            let slot = (cyclic_buffer_pos as usize) << 1;
            d[di] = 0;
            di += 1;
            assert!(slot + 1 < son_len);
            // SAFETY: tested just above.
            unsafe {
                tree.add(slot).write(K_EMPTY_HASH_VALUE);
                tree.add(slot + 1).write(K_EMPTY_HASH_VALUE);
            }
        } else {
            di += 1;
            let distances = di;
            debug_assert!((cyclic_buffer_pos as usize) < cyclic);
            debug_assert!(len_limit <= len_limit_0 + n && len_limit <= win.len());

            // C: `cur` in a register and every length an offset from it
            // (`LzFindOpt.asm:196-203`). `lim` is `len_limit - cur`, at least
            // `max_len_0 + 1` by condition C and the counters.
            // SAFETY: `cur < len_limit <= win.len()` ("the window").
            let at = unsafe { base.add(cur) };
            let lim = len_limit - cur;
            // SAFETY: "the tree": `cyclic_buffer_pos` is below the cyclic
            // size (assert A and the counters), so both are below
            // `2 * cyclic <= son.len()` (assert B).
            let (mut ptr0, mut ptr1) = unsafe {
                let slot = tree.add((cyclic_buffer_pos as usize) << 1);
                (slot.add(1), slot)
            };
            let mut cut = cut_value;
            let (mut len0, mut len1) = (0_usize, 0_usize);
            let mut max_len = max_len_0;

            loop {
                debug_assert!(delta != 0 && delta < cbs && cbs <= cyclic_buffer_size);
                // C: `cp_x -= delta; cmovb t0, cycSize; cp_x += t0`
                // (`LzFindOpt.asm:356-360`): the wrap as one borrow.
                let (back, wrapped) = cyclic_buffer_pos.overflowing_sub(delta);
                let back = back.wrapping_add(if wrapped { cbs } else { 0 });
                debug_assert!((back as usize) < cyclic);
                // SAFETY: "the tree": `back` is below the cyclic size, so
                // `2 * back + 1 < son.len()` (assert B).
                let pair = unsafe { tree.add((back as usize) << 1) };
                let diff = delta as usize;
                let mut len = if len0 < len1 { len0 } else { len1 };
                debug_assert!(len < lim && max_len < lim && diff <= cur);

                // SAFETY: "the tree", as above.
                let pair0 = unsafe { pair.read() };
                // SAFETY: "the window": `len < lim`, so `cur + len <
                // len_limit <= win.len()`, and `diff <= cur` (assert E and
                // the counters), so the older byte is at or after the
                // window's start.
                let (mut a, mut b) = unsafe { (at.add(len).sub(diff).read(), at.add(len).read()) };
                if a == b {
                    // SAFETY: as above: every byte the scan reads is below
                    // `cur + lim = len_limit` and at or after
                    // `cur + len + 1 - diff >= 0`.
                    len = unsafe { scan(at, diff, len + 1, lim) };
                    if max_len < len {
                        max_len = len;
                        d[di] = len as u32;
                        d[di + 1] = delta - 1;
                        di += 2;

                        if len == lim {
                            // SAFETY: "the tree", as above.
                            unsafe {
                                let pair1 = pair.add(1).read();
                                ptr1.write(pair0);
                                ptr0.write(pair1);
                            }
                            d[distances - 1] = (di - distances) as u32;

                            // C: `USE_LONG_MATCH_OPT`, `LzFindOpt.asm:453-507`.
                            if hi == hsize
                                || heads[hi] != delta
                                || win[len_limit - diff] != win[len_limit]
                                || di >= limit
                            {
                                break;
                            }
                            loop {
                                d[di] = 2;
                                d[di + 1] = (len_limit - cur) as u32;
                                d[di + 2] = delta - 1;
                                di += 3;
                                cur += 1;
                                len_limit += 1;
                                cyclic_buffer_pos += 1;
                                let dest = (cyclic_buffer_pos as usize) << 1;
                                let (back, wrapped) = cyclic_buffer_pos.overflowing_sub(delta);
                                let src = (back.wrapping_add(if wrapped { cbs } else { 0 })
                                    as usize)
                                    << 1;
                                assert!(dest + 1 < son_len && src + 1 < son_len);
                                // SAFETY: tested just above.
                                unsafe {
                                    let p0 = tree.add(src).read();
                                    let p1 = tree.add(src + 1).read();
                                    tree.add(dest).write(p0);
                                    tree.add(dest + 1).write(p1);
                                }
                                pos = pos.wrapping_add(1);
                                hi += 1;
                                if hi == hsize
                                    || heads[hi] != delta
                                    || win[len_limit - diff] != win[len_limit]
                                    || di >= limit
                                {
                                    break;
                                }
                            }
                            break;
                        }
                    }
                    debug_assert!(len < lim);
                    // SAFETY: "the window": the scan answered below `lim`
                    // (an answer equal to it left above, since
                    // `max_len < lim`), and at or above where it started.
                    (a, b) = unsafe { (at.add(len).sub(diff).read(), at.add(len).read()) };
                }
                let cur_match = pos.wrapping_sub(delta);
                if a < b {
                    // SAFETY: "the tree": `pair + 1` as above; `ptr1` is
                    // the slot's own or an earlier node's `pair + 1`.
                    unsafe {
                        delta = pair.add(1).read();
                        ptr1.write(cur_match);
                        ptr1 = pair.add(1);
                    }
                    len1 = len;
                } else {
                    delta = pair0;
                    // SAFETY: "the tree": `ptr0` is the slot's own or an
                    // earlier node's `pair`.
                    unsafe { ptr0.write(cur_match) };
                    ptr0 = pair;
                    len0 = len;
                }
                if delta >= cur_match {
                    return None;
                }
                delta = pos.wrapping_sub(delta);

                cut -= 1;
                if cut == 0 || delta >= cbs {
                    // SAFETY: "the tree", as above.
                    unsafe {
                        ptr0.write(K_EMPTY_HASH_VALUE);
                        ptr1.write(K_EMPTY_HASH_VALUE);
                    }
                    d[distances - 1] = (di - distances) as u32;
                    break;
                }
            }
        }
        pos = pos.wrapping_add(1);
        cyclic_buffer_pos += 1;
        cur += 1;
        if di >= limit {
            break;
        }
    }
    Some((di, pos))
}
