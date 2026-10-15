//! The bt thread's SDK tree walk with a checked margin wrapper.
//!
//! C: GetMatchesSpecN_2 in SDK 26.03's Asm/x86/LzFindOpt.asm and
//! C/LzFindOpt.c. All x86-64 variants use negative offsets from the match
//! limit and load the node's first child before comparing bytes. The earlier
//! bt_kernel_x86_64.S changes register allocation and child addressing, and
//! extends matches with bounded 16/8-byte comparisons and a byte tail.
//! bt_kernel_x86_64_sdk.S retains the complete upstream System V instruction
//! sequence for the fixed x86-64 candidate and differential tests. The sdk_wide
//! diagnostic combines that flow with the earlier bounded extension. All preserve
//! this port's two additional invalid-table checks. `scan` below uses the
//! same margin with eight-byte words on the portable path.
//!
//! The wrapper establishes the slice kernel's five bounds invariants and
//! reserves output for a complete position (F). Short output buffers use the
//! checked slice walk. Inputs outside the assembly's integer/direct-mode
//! contract use the Rust pointer walk below, which also runs on other targets
//! and under Miri. Native differential tests exercise the assembly against
//! the checked walk, comparing every tree/output write and every refusal.

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
#[inline(always)]
pub(crate) fn get_matches_spec_n_2_reg(
    win: &[u8],
    len_limit_0: usize,
    pos: u32,
    cur: usize,
    son: &mut [u32],
    cut_value: u32,
    d: &mut [u32],
    di: usize,
    max_len_0: usize,
    heads: &[u32],
    hi: usize,
    limit: usize,
    hsize: usize,
    cyclic_buffer_pos: u32,
    cyclic_buffer_size: u32,
) -> Option<(usize, u32)> {
    #[cfg(all(
        feature = "kernel-ab",
        not(miri),
        any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", target_os = "macos")
        )
    ))]
    match crate::kernel_ab::bt_variant() {
        1 => {
            return get_matches_spec_n_2_reg_variant::<1>(
                win,
                len_limit_0,
                pos,
                cur,
                son,
                cut_value,
                d,
                di,
                max_len_0,
                heads,
                hi,
                limit,
                hsize,
                cyclic_buffer_pos,
                cyclic_buffer_size,
            );
        }
        2 => {
            return get_matches_spec_n_2_reg_variant::<2>(
                win,
                len_limit_0,
                pos,
                cur,
                son,
                cut_value,
                d,
                di,
                max_len_0,
                heads,
                hi,
                limit,
                hsize,
                cyclic_buffer_pos,
                cyclic_buffer_size,
            );
        }
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        3 => {
            return get_matches_spec_n_2_reg_variant::<3>(
                win,
                len_limit_0,
                pos,
                cur,
                son,
                cut_value,
                d,
                di,
                max_len_0,
                heads,
                hi,
                limit,
                hsize,
                cyclic_buffer_pos,
                cyclic_buffer_size,
            );
        }
        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        4 => {
            return get_matches_spec_n_2_reg_variant::<4>(
                win,
                len_limit_0,
                pos,
                cur,
                son,
                cut_value,
                d,
                di,
                max_len_0,
                heads,
                hi,
                limit,
                hsize,
                cyclic_buffer_pos,
                cyclic_buffer_size,
            );
        }
        _ => {}
    }
    get_matches_spec_n_2_reg_variant::<
        {
            if cfg!(feature = "kernel-ab") {
                0
            } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
                4
            } else {
                1
            }
        },
    >(
        win,
        len_limit_0,
        pos,
        cur,
        son,
        cut_value,
        d,
        di,
        max_len_0,
        heads,
        hi,
        limit,
        hsize,
        cyclic_buffer_pos,
        cyclic_buffer_size,
    )
}

/// Shared margin proof for all assembly variants. Tests select each variant
/// explicitly, without process-global environment changes or races.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
#[inline(never)]
pub(crate) fn get_matches_spec_n_2_reg_variant<const VARIANT: u8>(
    win: &[u8],
    len_limit_0: usize,
    mut pos: u32,
    mut cur: usize,
    son: &mut [u32],
    cut_value: u32,
    d: &mut [u32],
    di: usize,
    max_len_0: usize,
    heads: &[u32],
    hi: usize,
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

    // C: LzFindOpt.asm keeps this length constant as both endpoints move.
    let lim = len_limit_0 - cur + 1;
    // F: a position reserves one count and at most `lim - max_len_0`
    // pairs (strictly increasing lengths). A long-match position uses three
    // words, also covered since C establishes `lim > max_len_0`.
    // Either the whole run fits, or every position started below `limit`
    // fits. The first position runs even when di >= limit.
    let room = (lim - max_len_0)
        .checked_mul(2)
        .and_then(|v| v.checked_add(1));
    let fits = room.is_some_and(|room| {
        n.checked_mul(room)
            .and_then(|v| di.checked_add(v))
            .is_some_and(|end| end <= d.len())
            || di
                .max(limit.saturating_sub(1))
                .checked_add(room)
                .is_some_and(|end| end <= d.len())
    });
    if !fits {
        return super::lz_find_mt::get_matches_spec_n_2(
            win,
            len_limit_0,
            pos,
            cur,
            son,
            cut_value,
            d,
            di,
            max_len_0,
            heads,
            hi,
            limit,
            hsize,
            cyclic_buffer_pos,
            cyclic_buffer_size,
        );
    }
    #[cfg(all(target_arch = "aarch64", target_os = "macos", not(miri)))]
    if (VARIANT == 2 || VARIANT == 3 || VARIANT == 4)
        && lim <= u32::MAX as usize
        && (!cfg!(debug_assertions) || cut_value != 0)
        && (pos as usize)
            .checked_add(n)
            .is_some_and(|end| end <= u32::MAX as usize)
        && (pos >= cyclic_buffer_size || pos == cyclic_buffer_pos)
    {
        let stop = d.as_ptr().addr().saturating_add(limit.saturating_mul(4));
        let mut pos_res = pos;
        // SAFETY: A/B bound tree nodes; C/D bound lookahead; E and the
        // descending-child checks bound backward reads; F reserves output.
        // The guards preserve 32-bit positions and debug cut-count overflow.
        // The assembly borrows these slices only for this call, preserves all
        // callee-saved registers, and writes only the exclusive son/d/pos_res.
        let walk = match VARIANT {
            4 => sdk_walk_arm64_negative,
            3 => sdk_walk_arm64_wide,
            _ => sdk_walk_arm64,
        };
        let end = unsafe {
            walk(
                win.as_ptr().add(len_limit_0),
                pos as usize,
                win.as_ptr().add(cur),
                son.as_mut_ptr(),
                cut_value,
                d.as_mut_ptr().add(di),
                max_len_0,
                heads.as_ptr().add(hi),
                stop,
                heads.as_ptr().add(hsize),
                cyclic_buffer_pos as usize,
                cyclic_buffer_size,
                &raw mut pos_res,
            )
        };
        if end.is_null() {
            return None;
        }
        // SAFETY: F confines the returned cursor to d or its one-past end.
        return Some((unsafe { end.offset_from(d.as_ptr()) } as usize, pos_res));
    }
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    if lim <= u32::MAX as usize
        // Preserve the checked kernel's debug overflow on a zero cut count.
        // Release arithmetic wraps like the SDK's decrement instruction.
        && (!cfg!(debug_assertions) || cut_value != 0)
        && (pos as usize)
            .checked_add(n)
            .is_some_and(|end| end <= u32::MAX as usize)
        && (pos >= cyclic_buffer_size || pos == cyclic_buffer_pos)
    {
        let stop = d.as_ptr().addr().saturating_add(limit.saturating_mul(4));
        let mut pos_res = pos;
        #[cfg(any(test, feature = "kernel-ab"))]
        let walk = match VARIANT {
            1 => sdk_walk_faithful,
            2 => sdk_walk_faithful_wide,
            _ => sdk_walk,
        };
        #[cfg(not(any(test, feature = "kernel-ab")))]
        let walk = if VARIANT == 1 {
            sdk_walk_faithful
        } else {
            sdk_walk
        };
        // SAFETY: A/B bound every tree node; C/D bound the current and
        // lookahead bytes; E and the assembly's descending-child checks bound
        // backward reads. F reserves all output writes. The guards above
        // establish the assembly's 32-bit length and position arithmetic and
        // its direct-mode mapping (logical position equals cyclic position).
        // All pointers borrow these slices for this call only; son/d are
        // exclusive and pos_res is initialized, exclusively owned storage.
        let end = unsafe {
            walk(
                win.as_ptr().add(len_limit_0),
                pos as usize,
                win.as_ptr().add(cur),
                son.as_mut_ptr(),
                cut_value,
                d.as_mut_ptr().add(di),
                max_len_0,
                heads.as_ptr().add(hi),
                stop,
                heads.as_ptr().add(hsize),
                cyclic_buffer_pos as usize,
                cyclic_buffer_size,
                &raw mut pos_res,
            )
        };
        if end.is_null() {
            return None;
        }
        // SAFETY: F kept the assembly's output cursor in d (or one past).
        return Some((unsafe { end.offset_from(d.as_ptr()) } as usize, pos_res));
    }
    let output = d.as_mut_ptr();
    // SAFETY: F puts di in the output, and the heads assertion puts both
    // head cursors in their slice (the end may be one past it).
    let (mut dist, mut head, head_end) = unsafe {
        (
            output.add(di),
            heads.as_ptr().add(hi),
            heads.as_ptr().add(hsize),
        )
    };
    // Use an address for the stopping threshold: limit may exceed d.len()
    // when F proved the whole run fits, so it need not form a valid pointer.
    let stop = output.addr().saturating_add(limit.saturating_mul(4));
    // Every subsequent tree access uses this base without reborrowing son.
    let tree = son.as_mut_ptr();
    let base = win.as_ptr();

    loop {
        if head == head_end {
            break;
        }
        // SAFETY: the heads assertion and head != head_end establish this
        // read and the advance, which may reach the one-past end.
        let mut delta = unsafe { head.read() };
        // SAFETY: same head bounds as the read above.
        head = unsafe { head.add(1) };
        if delta == 0 {
            return None;
        }

        let mut cbs = cyclic_buffer_size;
        if pos < cbs {
            if delta > pos {
                return None;
            }
            cbs = pos;
        }

        if delta >= cbs {
            let slot = (cyclic_buffer_pos as usize) << 1;
            // SAFETY: F covers the count word. A keeps the current slot
            // below cyclic; B covers both tree words.
            unsafe {
                dist.write(0);
                dist = dist.add(1);
                tree.add(slot).write(K_EMPTY_HASH_VALUE);
                tree.add(slot + 1).write(K_EMPTY_HASH_VALUE);
            }
        } else {
            let count = dist;
            // SAFETY: F reserves the count and all pairs for this position.
            dist = unsafe { dist.add(1) };
            let distances = dist;
            debug_assert!((cyclic_buffer_pos as usize) < cyclic);
            debug_assert!(cur + lim <= win.len());

            // C: `cur` in a register and every length an offset from it
            // (`LzFindOpt.asm:196-203`). `lim` is `len_limit - cur`, at least
            // `max_len_0 + 1` by condition C and the counters.
            // SAFETY: `cur < len_limit <= win.len()` ("the window").
            let at = unsafe { base.add(cur) };
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

                // C: the assembly issues this load as a prefetch and then
                // reuses its register for the byte comparison. Keeping the
                // child live across the scan consumes another loop register.
                // SAFETY: "the tree", as above; the node is initialized.
                unsafe { pair.read_volatile() };
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
                        // SAFETY: F reserves a pair for each new length.
                        unsafe {
                            dist.write(len as u32);
                            dist.add(1).write(delta - 1);
                            dist = dist.add(2);
                        }

                        if len == lim {
                            // SAFETY: "the tree", as above.
                            unsafe {
                                let pair1 = pair.add(1).read();
                                ptr1.write(pair.read());
                                ptr0.write(pair1);
                            }
                            // SAFETY: F reserves count; both cursors are in the
                            // same output allocation and dist follows distances.
                            unsafe { count.write(dist.offset_from(distances) as u32) };

                            // C: `USE_LONG_MATCH_OPT`, `LzFindOpt.asm:453-507`.
                            // SAFETY: head != head_end means another position
                            // exists; D then puts cur + lim strictly in win.
                            // E gives diff <= cur, so the older byte fits too.
                            if head == head_end
                                || unsafe { head.read() } != delta
                                || unsafe {
                                    base.add(cur + lim - diff).read() != base.add(cur + lim).read()
                                }
                                || dist.addr() >= stop
                            {
                                break;
                            }
                            // Each repeated match emits exactly three words.
                            // Fold the output threshold into the head bound so
                            // the inner loop has only one remaining-count test.
                            let output_positions = (stop - dist.addr() - 1) / 12 + 1;
                            // SAFETY: both head cursors belong to heads; the
                            // new bound never exceeds its established end.
                            let repeat_end = unsafe {
                                head.add(output_positions.min(head_end.offset_from(head) as usize))
                            };
                            // At distance one, every destination copies the
                            // node just written. Keep that value across the
                            // run instead of a load dependent on each store.
                            let repeated_node = if delta == 1 {
                                // SAFETY: A and B cover the current node's two
                                // initialized words; u64 alignment is not needed.
                                unsafe {
                                    tree.add((cyclic_buffer_pos as usize) << 1)
                                        .cast::<u64>()
                                        .read_unaligned()
                                }
                            } else {
                                0
                            };
                            loop {
                                // SAFETY: F covers this position's triple.
                                unsafe {
                                    dist.write(2);
                                    dist.add(1).write(lim as u32);
                                    dist.add(2).write(delta - 1);
                                    dist = dist.add(3);
                                }
                                cur += 1;
                                cyclic_buffer_pos += 1;
                                let dest = (cyclic_buffer_pos as usize) << 1;
                                let (back, wrapped) = cyclic_buffer_pos.overflowing_sub(delta);
                                let src = (back.wrapping_add(if wrapped { cbs } else { 0 })
                                    as usize)
                                    << 1;
                                // SAFETY: A covers the next position and B
                                // its two tree words; the wrapped source is
                                // below cyclic by the same tree invariant. The
                                // u64 copy needs only u32 alignment and preserves
                                // both words regardless of native byte order.
                                unsafe {
                                    let node = if delta == 1 {
                                        repeated_node
                                    } else {
                                        tree.add(src).cast::<u64>().read_unaligned()
                                    };
                                    tree.add(dest).cast::<u64>().write_unaligned(node);
                                }
                                pos = pos.wrapping_add(1);
                                // SAFETY: the preceding condition established
                                // another head; advancing may reach head_end.
                                head = unsafe { head.add(1) };
                                // SAFETY: another head implies another window
                                // byte by D, and E bounds the backward read.
                                if head == repeat_end
                                    || unsafe { head.read() } != delta
                                    || unsafe {
                                        base.add(cur + lim - diff).read()
                                            != base.add(cur + lim).read()
                                    }
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
                    // SAFETY: "the tree": pair names the validated node.
                    delta = unsafe { pair.read() };
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

                // C: cutValueCur_VAR is a stack counter, leaving registers
                // for the dependent tree pointers.
                // SAFETY: cut is initialized and exclusively owned here.
                let remaining = unsafe { core::ptr::read_volatile(&cut) } - 1;
                // SAFETY: the same initialized, exclusively owned counter.
                unsafe { core::ptr::write_volatile(&mut cut, remaining) };
                if remaining == 0 || delta >= cbs {
                    // SAFETY: "the tree", as above.
                    unsafe {
                        ptr0.write(K_EMPTY_HASH_VALUE);
                        ptr1.write(K_EMPTY_HASH_VALUE);
                    }
                    // SAFETY: F reserves count; both cursors are in the
                    // same output allocation and dist follows distances.
                    unsafe { count.write(dist.offset_from(distances) as u32) };
                    break;
                }
            }
        }
        pos = pos.wrapping_add(1);
        cyclic_buffer_pos += 1;
        cur += 1;
        if dist.addr() >= stop {
            break;
        }
    }
    // SAFETY: F kept both pointers in the output allocation.
    Some((unsafe { dist.offset_from(output) } as usize, pos))
}

/// C: GetMatchesSpecN_2 in SDK 26.03's C/LzFindOpt.c, ARM64 register map.
///
/// # Safety
/// Requires the A-F margins and integer guards established by the wrapper.
/// Native differential tests cover mutations and refusals; Miri uses Rust.
#[cfg(all(target_arch = "aarch64", target_os = "macos", not(miri)))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "C" fn sdk_walk_arm64(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_aarch64_sdk.S"), options(raw))
}

/// SDK ARM64 flow retaining the portable walk's bounded word scan and early
/// child fetch. Diagnostic only; the unmodified SDK variant remains available.
///
/// # Safety
/// Requires the same A-F margins and integer guards as `sdk_walk_arm64`.
#[cfg(all(target_arch = "aarch64", target_os = "macos", not(miri)))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "C" fn sdk_walk_arm64_wide(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_aarch64_sdk_wide.S"), options(raw))
}

/// SDK GetMatchesSpecN_2 negative-offset control flow ported to ARM64.
///
/// # Safety
/// Requires the same A-F margins and integer guards as `sdk_walk_arm64`.
/// The native register-walk differential suite checks this diagnostic port.
#[cfg(all(target_arch = "aarch64", target_os = "macos", not(miri)))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "C" fn sdk_walk_arm64_negative(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_aarch64_negative.S"), options(raw))
}

/// C: GetMatchesSpecN_2 in SDK 26.03's LzFindOpt.asm, System V register map.
///
/// # Safety
///
/// The caller must establish A-F and the representability/direct-mode guards
/// in get_matches_spec_n_2_reg. The routine saves all System V callee-saved
/// registers and uses its own aligned stack frame. Miri uses the Rust walk;
/// native differential tests exercise these same margins with this assembly.
#[cfg(all(target_arch = "x86_64", not(miri)))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "sysv64" fn sdk_walk(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_x86_64.S"), options(raw))
}

/// Complete SDK GetMatchesSpecN_2 expansion, sharing the same checked entry
/// contract as the retained earlier kernel.
///
/// # Safety
///
/// Identical A-F, integer and direct-mode preconditions to `sdk_walk` above.
/// The same wrapper establishes them before either variant is entered.
#[cfg(all(target_arch = "x86_64", not(miri)))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "sysv64" fn sdk_walk_faithful(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_x86_64_sdk.S"), options(raw))
}

/// SDK GetMatchesSpecN_2 flow with bounded match extension for tests and
/// the non-default kernel-ab measurement feature.
///
/// # Safety
///
/// Identical A-F, integer and direct-mode preconditions to `sdk_walk` above.
/// The same wrapper establishes them before either variant is entered.
#[cfg(all(target_arch = "x86_64", not(miri), any(test, feature = "kernel-ab")))]
#[allow(clippy::too_many_arguments)]
#[unsafe(naked)]
unsafe extern "sysv64" fn sdk_walk_faithful_wide(
    len_limit: *const u8,
    pos: usize,
    cur: *const u8,
    son: *mut u32,
    cut: u32,
    d: *mut u32,
    max_len: usize,
    heads: *const u32,
    stop: usize,
    heads_end: *const u32,
    cyclic_pos: usize,
    cyclic_size: u32,
    pos_res: *mut u32,
) -> *mut u32 {
    core::arch::naked_asm!(include_str!("bt_kernel_x86_64_sdk_wide.S"), options(raw))
}
