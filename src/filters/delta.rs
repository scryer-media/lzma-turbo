//! The delta filter, both directions.
//!
//! C: `C/Delta.c`, `Delta_Decode` and `Delta_Encode`. Spec §5.3.3: the properties byte is the
//! distance minus one, so distances run from 1 to 256, and the filter adds
//! the byte `distance` back to each byte in turn.
//!
//! The C keeps its history in a `DELTA_STATE_SIZE` (256) byte buffer and
//! rotates it; this port keeps the same buffer and the same rotation, because
//! the alternative — a ring with a moving index — changes the inner loop.

use crate::error::XzErrorKind;

/// C: `DELTA_STATE_SIZE`.
const DELTA_STATE_SIZE: usize = 256;

/// A delta filter carrying the last `distance` bytes between calls.
#[derive(Debug, Clone, Copy)]
pub struct Delta {
    distance: usize,
    state: [u8; DELTA_STATE_SIZE],
}

impl Delta {
    /// A decoder for the one-byte filter property, which is the distance
    /// minus one.
    ///
    /// # Errors
    ///
    /// Never, for a one-byte property: every value 0-255 is a valid distance.
    /// The signature returns a result so that the caller's chain builder can
    /// treat every filter the same way.
    pub fn new(props: u8) -> Result<Self, XzErrorKind> {
        Ok(Delta {
            distance: usize::from(props) + 1,
            state: [0u8; DELTA_STATE_SIZE],
        })
    }

    /// The distance in bytes, 1 to 256.
    #[must_use]
    pub fn distance(&self) -> usize {
        self.distance
    }

    /// Decodes `data` in place. Every byte is consumed: the delta filter has
    /// no lookahead and no alignment, so it never leaves a tail.
    pub fn decode(&mut self, data: &mut [u8]) {
        // C: Delta_Decode(state, delta, data, size).
        let size = data.len();
        if size == 0 {
            return;
        }
        let delta = self.distance;

        if delta == 1 {
            // Delta_Decode's recurrence needs only the preceding decoded
            // byte. Keep it in a local instead of reloading it from output.
            let mut previous = self.state[0];
            for byte in data {
                previous = previous.wrapping_add(*byte);
                *byte = previous;
            }
            self.state[0] = previous;
            return;
        }

        if delta == 4 {
            // Delta_Decode has four independent recurrences at this distance.
            // Keep their preceding bytes together across complete groups.
            let mut previous: [u8; 4] = self.state[..4].try_into().expect("four-byte history");
            let mut groups = data.chunks_exact_mut(4);
            for group in groups.by_ref() {
                for (byte, prior) in group.iter_mut().zip(&mut previous) {
                    *prior = prior.wrapping_add(*byte);
                    *byte = *prior;
                }
            }
            for byte in groups.into_remainder() {
                *byte = byte.wrapping_add(previous[0]);
                previous.rotate_left(1);
                previous[3] = *byte;
            }
            self.state[..4].copy_from_slice(&previous);
            return;
        }

        if size <= delta {
            for (i, b) in data.iter_mut().enumerate() {
                *b = b.wrapping_add(self.state[i]);
            }
            // C: `for (; delta != i; state++, delta--) *state = state[i];`
            // — shift the unconsumed history down past what was just used.
            self.state.copy_within(size..delta, 0);
            let keep = delta - size;
            self.state[keep..delta].copy_from_slice(data);
        } else {
            // C: `for (i = 0; i < delta; i++) buf[i] = (Byte)(buf[i] + state[i]);`
            // - the index is the same one into two different buffers, so an
            // iterator over one of them does not express it.
            #[allow(clippy::needless_range_loop)]
            for i in 0..delta {
                data[i] = data[i].wrapping_add(self.state[i]);
            }
            // C: `for (i = delta; i < size; i++) buf[i] += buf[i - delta];`
            //
            // Written the way the C writes it this is a byte at a time, and it
            // has to be: `delta` is a runtime value, so nothing may assume the
            // distance between the two indices. But the recurrence only
            // reaches back `delta` bytes, so any `delta` consecutive outputs
            // depend on bytes that are already final and on nothing inside
            // their own block. Adding a block at a time says exactly that, as
            // two slices that cannot overlap, and the add vectorizes.
            //
            // Only from 16 bytes up. Below that the block is shorter than a
            // vector register and the per-block bookkeeping costs more than
            // the byte loop it replaces.
            #[cfg(feature = "kernel-ab")]
            let blocked = delta >= 16 && crate::kernel_ab::delta_blocked();
            #[cfg(not(feature = "kernel-ab"))]
            let blocked = delta >= 16;
            if blocked {
                let mut i = delta;
                while i < size {
                    let n = delta.min(size - i);
                    let (done, rest) = data.split_at_mut(i);
                    let src = &done[i - delta..i - delta + n];
                    for (d, s) in rest[..n].iter_mut().zip(src) {
                        *d = d.wrapping_add(*s);
                    }
                    i += n;
                }
            } else {
                for i in delta..size {
                    data[i] = data[i].wrapping_add(data[i - delta]);
                }
            }
            self.state[..delta].copy_from_slice(&data[size - delta..]);
        }
    }

    /// Encodes `data` in place, the inverse of [`decode`](Self::decode) and
    /// likewise consuming every byte.
    ///
    /// C: `Delta_Encode(state, delta, data, size)`.
    pub fn encode(&mut self, data: &mut [u8]) {
        let size = data.len();
        if size == 0 {
            return;
        }
        let delta = self.distance;

        // C: `Byte temp[DELTA_STATE_SIZE]` — the encoder needs the old history
        // after it has already overwritten `state` with the new one.
        let mut temp = [0u8; DELTA_STATE_SIZE];
        temp[..delta].copy_from_slice(&self.state[..delta]);

        if size <= delta {
            // C: the `do { b = *data; *data++ = b - temp[i]; temp[i] = b; }`
            // loop, then the rotation of `temp` by `size` into `state`.
            for (i, b) in data.iter_mut().enumerate() {
                let old = *b;
                *b = old.wrapping_sub(temp[i]);
                temp[i] = old;
            }
            let mut i = size;
            for slot in self.state[..delta].iter_mut() {
                if i == delta {
                    i = 0;
                }
                *slot = temp[i];
                i += 1;
            }
        } else {
            // C: the new history is taken before the data is overwritten, and
            // the subtraction then walks *backwards*, so that each byte still
            // sees its unencoded predecessor.
            self.state[..delta].copy_from_slice(&data[size - delta..]);
            for i in (delta..size).rev() {
                data[i] = data[i].wrapping_sub(data[i - delta]);
            }
            // C: `do { --p; *p -= temp[--dif]; } while (dif != 0);`
            #[allow(clippy::needless_range_loop)]
            for i in (0..delta).rev() {
                data[i] = data[i].wrapping_sub(temp[i]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The spec's own reference encoder (§5.3.3.1), so the test is against the
    /// document rather than against this file.
    fn spec_encode(distance: usize, data: &[u8]) -> Vec<u8> {
        let mut delta = [0u8; 256];
        let mut pos: u8 = 0;
        let mut out = Vec::with_capacity(data.len());
        for &byte in data {
            let tmp = delta[usize::from((distance as u8).wrapping_add(pos))];
            let tmp = byte.wrapping_sub(tmp);
            delta[usize::from(pos)] = byte;
            out.push(tmp);
            pos = pos.wrapping_sub(1);
        }
        out
    }

    #[test]
    fn decodes_what_the_spec_encoder_produced() {
        let data: Vec<u8> = (0u32..5000).map(|i| (i * 37 + 11) as u8).collect();
        for distance in [1usize, 2, 3, 4, 16, 255, 256] {
            let encoded = spec_encode(distance, &data);
            // In one call.
            let mut buf = encoded.clone();
            Delta::new((distance - 1) as u8)
                .expect("props")
                .decode(&mut buf);
            assert_eq!(buf, data, "distance {distance}");

            // And split every which way, which is what exercises the state.
            for chunk in [1usize, 5, 256, 257, 1000] {
                let mut d = Delta::new((distance - 1) as u8).expect("props");
                let mut out = Vec::new();
                for part in encoded.chunks(chunk) {
                    let mut piece = part.to_vec();
                    d.decode(&mut piece);
                    out.extend_from_slice(&piece);
                }
                assert_eq!(out, data, "distance {distance}, chunk {chunk}");
            }
        }
    }

    #[test]
    fn encoding_then_decoding_gives_the_input_back() {
        for distance in [1usize, 2, 5, 256] {
            for len in [0usize, 1, 3, 255, 256, 257, 4096] {
                let src: Vec<u8> = (0..len).map(|i| (i * 7 + i / 3) as u8).collect();
                let mut buf = src.clone();
                Delta::new((distance - 1) as u8).unwrap().encode(&mut buf);
                Delta::new((distance - 1) as u8).unwrap().decode(&mut buf);
                assert_eq!(buf, src, "distance {distance}, {len} bytes");
            }
        }
    }

    #[test]
    fn encoding_in_pieces_is_encoding_whole() {
        // The filter carries `distance` bytes of history between calls, so a
        // chunked encode must be the same as a single one.
        let src: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        for distance in [1usize, 4, 256] {
            let mut whole = src.clone();
            Delta::new((distance - 1) as u8).unwrap().encode(&mut whole);
            for chunk in [1usize, 3, 256, 1000] {
                let mut d = Delta::new((distance - 1) as u8).unwrap();
                let mut out: Vec<u8> = Vec::new();
                for piece in src.chunks(chunk) {
                    let mut buf = piece.to_vec();
                    d.encode(&mut buf);
                    out.extend_from_slice(&buf);
                }
                assert_eq!(out, whole, "distance {distance}, chunk {chunk}");
            }
        }
    }
}
