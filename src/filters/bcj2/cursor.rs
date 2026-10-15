//! Complete pointer-cursor port of C `Bcj2Dec_Decode`, diagnostic until gated.

use super::*;

struct Cursors {
    bufs: [*const u8; NUM_STREAMS],
    lims: [*const u8; NUM_STREAMS],
    dest: *mut u8,
    dest_lim: *mut u8,
}

pub(super) fn decode(p: &mut Bcj2Dec, s: &mut Bcj2DecStreams<'_>) -> Result<(), Bcj2Error> {
    if !s.bufs[STREAM_CALL].len().is_multiple_of(4)
        || !s.bufs[STREAM_JUMP].len().is_multiple_of(4)
        || s.dest_pos > s.dest.len()
    {
        return p.decode_checked(s);
    }
    let starts = s.bufs.map(<[u8]>::as_ptr);
    let dest_start = s.dest.as_mut_ptr();
    // SAFETY: each start/limit belongs to its borrowed slice, including empty
    // slices. The exclusive destination cannot overlap any borrowed input or
    // decoder state. Entry checks establish whole side-stream words and a
    // valid destination cursor. The kernel advances only under its per-stream
    // limits, and returns cursors within the original slices even on errors.
    unsafe {
        let mut c = Cursors {
            bufs: starts,
            lims: core::array::from_fn(|i| starts[i].add(s.bufs[i].len())),
            dest: dest_start.add(s.dest_pos),
            dest_lim: dest_start.add(s.dest.len()),
        };
        let result = decode_inner(p, &mut c);
        for (i, start) in starts.into_iter().enumerate() {
            s.bufs[i] = &s.bufs[i][c.bufs[i].offset_from(start) as usize..];
        }
        s.dest_pos = c.dest.offset_from(dest_start) as usize;
        result
    }
}

/// C: `Bcj2Dec_Decode`. Cursor construction and the whole-word side-stream
/// invariant are checked by `decode`; all movement below is limit-checked.
#[allow(clippy::too_many_lines)]
unsafe fn decode_inner(p: &mut Bcj2Dec, c: &mut Cursors) -> Result<(), Bcj2Error> {
    // SAFETY: input cursor/limit pairs and destination belong to their own
    // slices. Reads/writes occur only after checking the corresponding limit.
    // CALL/JUMP cursors advance by four, so nonempty means a complete word.
    // The four-way scan uses min(input, output) rounded down to four; its
    // scalar tail checks both ends. Pointer differences use the same slice.
    unsafe {
        let mut v = p.temp;
        if p.range <= 5 {
            let mut code = p.code;
            p.state = DEC_STATE_ERROR;
            while p.range != 5 {
                if p.range == 1 && code != 0 {
                    return Err(Bcj2Error::Data);
                }
                if c.bufs[STREAM_RC] == c.lims[STREAM_RC] {
                    p.state = STREAM_RC;
                    return Ok(());
                }
                code = (code << 8) | u32::from(*c.bufs[STREAM_RC]);
                c.bufs[STREAM_RC] = c.bufs[STREAM_RC].add(1);
                p.code = code;
                p.range += 1;
            }
            if code == u32::MAX {
                return Err(Bcj2Error::Data);
            }
            p.range = u32::MAX;
        }
        let mut state = p.state;
        if is_32bit_stream(state) {
            let cur = c.bufs[state];
            if cur == c.lims[state] {
                return Ok(());
            }
            c.bufs[state] = cur.add(4);
            p.ip = p.ip.wrapping_add(4);
            v = u32::from_be(cur.cast::<u32>().read_unaligned()).wrapping_sub(p.ip);
            state = DEC_STATE_ORIG_0;
        }
        if state.wrapping_sub(DEC_STATE_ORIG_0) < 4 {
            loop {
                if c.dest == c.dest_lim {
                    p.state = state;
                    p.temp = v;
                    return Ok(());
                }
                *c.dest = v as u8;
                c.dest = c.dest.add(1);
                state += 1;
                if state == DEC_STATE_ORIG_3 + 1 {
                    break;
                }
                v >>= 8;
            }
        }
        loop {
            if p.range < TOP_VALUE {
                if c.bufs[STREAM_RC] == c.lims[STREAM_RC] {
                    p.state = STREAM_RC;
                    p.temp = v;
                    return Ok(());
                }
                p.range <<= 8;
                p.code = (p.code << 8) | u32::from(*c.bufs[STREAM_RC]);
                c.bufs[STREAM_RC] = c.bufs[STREAM_RC].add(1);
            }
            let mut src = c.bufs[STREAM_MAIN];
            let mut dest = c.dest;
            let num = (c.lims[STREAM_MAIN].offset_from(src) as usize)
                .min(c.dest_lim.offset_from(dest) as usize)
                & !3;
            let src_lim = src.add(num);
            'scan: {
                macro_rules! one_iter {
                    ($index:expr) => {{
                        let b = u32::from(*src.add($index));
                        *dest = b as u8;
                        dest = dest.add(1);
                        v = (v << 24) | b;
                        if is_branch_byte(b) || is_long_jump(v) {
                            break 'scan;
                        }
                    }};
                }
                while src != src_lim {
                    one_iter!(0);
                    one_iter!(1);
                    one_iter!(2);
                    one_iter!(3);
                    src = src.add(4);
                }
                loop {
                    if src == c.lims[STREAM_MAIN] || dest == c.dest_lim {
                        let consumed = src.offset_from(c.bufs[STREAM_MAIN]) as u32;
                        c.bufs[STREAM_MAIN] = src;
                        c.dest = dest;
                        p.ip = p.ip.wrapping_add(consumed);
                        p.state = if src == c.lims[STREAM_MAIN] {
                            STREAM_MAIN
                        } else {
                            DEC_STATE_ORIG
                        };
                        p.temp = v;
                        return Ok(());
                    }
                    one_iter!(0);
                    src = src.add(1);
                }
            }
            let consumed = dest.offset_from(c.dest) as usize;
            c.dest = dest;
            c.bufs[STREAM_MAIN] = c.bufs[STREAM_MAIN].add(consumed);
            p.ip = p.ip.wrapping_add(consumed as u32);
            let prob = &mut p.probs[prob_index(v)];
            let ttt = u32::from(*prob);
            let bound = (p.range >> NUM_BIT_MODEL_TOTAL_BITS) * ttt;
            if p.code < bound {
                p.range = bound;
                *prob = (ttt + ((BIT_MODEL_TOTAL - ttt) >> NUM_MOVE_BITS)) as u16;
                continue;
            }
            p.range -= bound;
            p.code -= bound;
            *prob = (ttt - (ttt >> NUM_MOVE_BITS)) as u16;
            let cj = branch_stream(v);
            let cur = c.bufs[cj];
            if cur == c.lims[cj] {
                p.state = cj;
                break;
            }
            v = u32::from_be(cur.cast::<u32>().read_unaligned());
            c.bufs[cj] = cur.add(4);
            p.ip = p.ip.wrapping_add(4);
            v = v.wrapping_sub(p.ip);
            let rem = c.dest_lim.offset_from(c.dest) as usize;
            if rem < 4 {
                for _ in 0..rem {
                    *c.dest = v as u8;
                    c.dest = c.dest.add(1);
                    v >>= 8;
                }
                p.temp = v;
                p.state = DEC_STATE_ORIG_0 + rem;
                break;
            }
            c.dest.cast::<u32>().write_unaligned(v.to_le());
            v >>= 24;
            c.dest = c.dest.add(4);
        }
        if p.range < TOP_VALUE && c.bufs[STREAM_RC] != c.lims[STREAM_RC] {
            p.range <<= 8;
            p.code = (p.code << 8) | u32::from(*c.bufs[STREAM_RC]);
            c.bufs[STREAM_RC] = c.bufs[STREAM_RC].add(1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare_step(
        checked: &mut Bcj2Dec,
        cursor: &mut Bcj2Dec,
        input: [&[u8]; 4],
        output_len: usize,
    ) -> ([usize; 4], Vec<u8>, bool) {
        let mut want = vec![0xcc; output_len + 2];
        let mut got = want.clone();
        let mut a = Bcj2DecStreams::new(
            input[0],
            input[1],
            input[2],
            input[3],
            &mut want[1..output_len + 1],
        );
        let mut b = Bcj2DecStreams::new(
            input[0],
            input[1],
            input[2],
            input[3],
            &mut got[1..output_len + 1],
        );
        let expected = checked.decode_checked(&mut a);
        let actual = decode(cursor, &mut b);
        assert_eq!(expected, actual);
        assert_eq!(a.dest_pos, b.dest_pos);
        assert_eq!(a.bufs, b.bufs);
        assert_eq!(
            (
                checked.state,
                checked.ip,
                checked.temp,
                checked.range,
                checked.code
            ),
            (
                cursor.state,
                cursor.ip,
                cursor.temp,
                cursor.range,
                cursor.code
            )
        );
        assert_eq!(checked.probs, cursor.probs);
        let used = core::array::from_fn(|i| input[i].len() - a.bufs[i].len());
        let produced = a.dest_pos;
        assert_eq!(want, got);
        (used, got[1..produced + 1].to_vec(), actual.is_err())
    }

    #[test]
    fn sdk_cursor_matches_checked_at_every_stream_and_output_edge() {
        let mut data = Vec::new();
        for i in 0..if cfg!(miri) { 12u32 } else { 257 } {
            data.extend_from_slice(&[0x90, 0xe8]);
            data.extend_from_slice(&(i * 13).to_le_bytes());
            data.extend_from_slice(&[0x0f, 0x84]);
            data.extend_from_slice(&(i * 7).to_le_bytes());
            data.extend_from_slice(&[0xe9, 0, 0, 0, 0, 0x90]);
        }
        let encoded = encode_to_streams(&data);
        let inputs = [encoded.main, encoded.call, encoded.jump, encoded.rc];
        for step in [1usize, 3, 4, 17, 64] {
            for out_step in [1, 2, 3, 4, 5, 31, 128] {
                let mut checked = Bcj2Dec::new();
                let mut cursor = checked.clone();
                let mut offsets = [0; 4];
                let mut output = Vec::new();
                for call in 0..data.len() * 2 + 20 {
                    let input = core::array::from_fn(|i| {
                        let end = inputs[i].len().min((call + 1) * step);
                        let end = if is_32bit_stream(i) { end & !3 } else { end };
                        &inputs[i][offsets[i]..end]
                    });
                    // An empty output call can still consume a converted
                    // target or range normalization. Compare that state too.
                    let size = if call % 7 == 0 { 0 } else { out_step };
                    let (used, produced, failed) =
                        compare_step(&mut checked, &mut cursor, input, size);
                    assert!(!failed);
                    for i in 0..4 {
                        offsets[i] += used[i];
                    }
                    output.extend_from_slice(&produced);
                    if output.len() == data.len() {
                        break;
                    }
                }
                assert_eq!(output, data, "input step {step}, output step {out_step}");
            }
        }
    }

    #[test]
    fn sdk_cursor_matches_checked_for_malformed_streams() {
        let mut rng = 0x1234_5678u32;
        for case in 0..if cfg!(miri) { 12 } else { 2048 } {
            let mut bytes = [0u8; 132];
            for b in &mut bytes {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                *b = rng as u8;
            }
            if case % 2 == 0 {
                bytes[96] = 0;
            }
            let call_len = if case % 11 == 0 { 15 } else { (case % 9) * 4 };
            let jump_len = (case % 8) * 4;
            let inputs = [
                &bytes[..case % 33],
                &bytes[32..32 + call_len],
                &bytes[64..64 + jump_len],
                &bytes[96..96 + case % 33],
            ];
            let mut checked = Bcj2Dec::with_ip(u32::MAX - case as u32);
            let mut cursor = checked.clone();
            let mut offsets = [0; 4];
            for step in 0..9 {
                let input = core::array::from_fn(|i| &inputs[i][offsets[i]..]);
                let (used, _, failed) = compare_step(&mut checked, &mut cursor, input, step);
                for i in 0..4 {
                    offsets[i] += used[i];
                }
                if failed {
                    break;
                }
            }
        }
    }
}
