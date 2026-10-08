//! Embedder-supplied delegation hooks for the bulk checksums and hashes.
//!
//! With `crc-host` and/or `crypto-host` enabled, a **wasm** build of this crate
//! does not run the container's bulk checksum primitives itself: it calls a set
//! of plain Rust function pointers that the embedding program installs at
//! start-up. Whatever sits behind the pointers - a raw wasm import in some
//! namespace, a `wit-bindgen`-generated component import, a host SDK call - is
//! the embedder's business. `lzma-turbo` takes no dependency on any runtime,
//! SDK or interface definition; it only calls the `fn` pointers it was handed.
//!
//! The point is speed. `crc-fast` reaches its published throughput through the
//! carry-less multiply units, and wasm has no `pclmulqdq`, no `vmull_p64` and
//! no equivalent; `sha2` on wasm likewise runs the plain 32-bit compression
//! function with no `sha256rnds2` or `sha256h` behind it. A host that already
//! has those instruction sets can do the whole bulk checksum of an `.xz` stream
//! an order of magnitude faster than the guest can, and the guest hands it
//! nothing but a byte range it already owns.
//!
//! This module is present whenever at least one of `crc-host` / `crypto-host`
//! is enabled. Only the hooks a feature consumes are ever called: with
//! `crc-host` alone the SHA-256 hooks are never invoked, and with
//! `crypto-host` alone the CRC hooks are not. On native targets the features
//! are accepted but the in-process backends stay active, so installed hooks are
//! never called there - which means feature unification in a mixed workspace
//! cannot silently turn a native build into a delegating one.
//!
//! Both features imply `std`: the registry below is a `std::sync::RwLock`.
//!
//! ## The seam
//!
//! ```ignore
//! use lzma_turbo::hooks::{HostHashHooks, HostSha256Handle, install_host_hash_hooks};
//!
//! install_host_hash_hooks(HostHashHooks {
//!     crc32: |seed, data| /* forward to the embedder's CRC-32 */,
//!     crc64_xz: |seed, data| /* forward to the embedder's CRC-64/XZ */,
//!     sha256_init: || /* a fresh host SHA-256 state */,
//!     sha256_clone: |handle| /* an independent copy of that state */,
//!     sha256_update: |handle, data| /* append bytes */,
//!     sha256_finalize: |handle| /* the 32-byte digest; releases the handle */,
//!     sha256_drop: |handle| /* release without a digest */,
//! });
//! ```
//!
//! `examples/wasm_xz_conformance.rs` is a complete reference embedding: a
//! `wasm32-wasip1` guest that declares raw imports in a `host` namespace and
//! installs hooks that forward to them, driven by the native `wasmtime` harness
//! in `tools/wasm-conformance/tests/wasm_host_conformance.rs`.
//!
//! ## Contract the hooks must satisfy
//!
//! ### The CRCs
//!
//! Both CRC hooks are **resumable one-shots in the finalized domain**. That is
//! the only convention with no hidden state, and it is what makes the
//! streaming digests in [`crate::crc`] a running integer rather than a host
//! object:
//!
//! * `crc32(seed, data) -> u32` is CRC-32/ISO-HDLC: reflected, polynomial
//!   `0xEDB8_8320`, initial state `0xFFFF_FFFF`, final xor `0xFFFF_FFFF`.
//! * `crc64_xz(seed, data) -> u64` is CRC-64/XZ: reflected, polynomial
//!   `0xC96C_5795_D787_0F42`, initial state `!0`, final xor `!0`.
//!
//! For each, writing `C(s, d)` for the hook:
//!
//! 1. **Zero seeds the stream.** `C(0, d)` is the ordinary checksum of `d`, the
//!    value the container stores. In particular `C(0, &[]) == 0`.
//! 2. **Empty updates are identities.** `C(s, &[]) == s` for every `s`.
//! 3. **It chains.** `C(C(0, a), b) == C(0, a ++ b)`, for any split, at any
//!    depth.
//!
//! Point 3 is the load-bearing one: it is what lets [`crate::crc::Crc32`] and
//! [`crate::crc::Crc64Xz`] carry a plain integer across `update` calls. A host
//! whose CRC library exposes only a running (pre-final-xor) register seeds it
//! with `!seed` and applies the final xor on the way out; the reference host in
//! `tools/wasm-conformance/tests/wasm_host_conformance.rs` does exactly that with `crc-fast`'s
//! `Digest::new_with_init_state`, and the native test at the bottom of
//! [`crate::crc`] proves the three properties above through the real registry.
//!
//! Note what is *not* delegated: [`crate::crc::crc32_combine`] and
//! [`crate::crc::crc64_xz_combine`]. Folding two checksums is arithmetic on
//! two integers and a length, not a pass over data, so crossing a boundary for
//! it would be pure overhead. They stay on `crc-fast` on every target.
//!
//! ### SHA-256
//!
//! SHA-256 has no seeded-resume form, so it is delegated as a **streaming
//! state behind an opaque handle**. This is what lets a multi-gigabyte block be
//! hashed incrementally, in whatever chunks the decoder produces, without the
//! guest buffering it or the host being handed the whole thing at once.
//!
//! * `sha256_init() -> HostSha256Handle` creates a fresh state and returns a
//!   handle that is **live** until it is passed to `sha256_finalize` or
//!   `sha256_drop`. The value is opaque to this crate: an index, a pointer, a
//!   generation counter, anything the embedder likes.
//! * `sha256_clone(handle) -> HostSha256Handle` returns a second, independent
//!   live handle whose state equals `handle`'s at the moment of the call.
//!   Neither handle may affect the other afterwards.
//! * `sha256_update(handle, data)` appends `data`. An empty `data` is legal and
//!   changes nothing.
//! * `sha256_finalize(handle) -> [u8; 32]` returns the digest of everything
//!   appended so far and **consumes** `handle`: the embedder releases it, and
//!   this crate never passes it again.
//! * `sha256_drop(handle)` releases `handle` without producing a digest. This
//!   crate calls it for every live handle it does not finalize, so a host may
//!   treat a leaked handle as a bug rather than a normal outcome.
//!
//! Every handle this crate creates is passed to exactly one of `sha256_finalize`
//! or `sha256_drop`, exactly once. A host may therefore make a stale handle a
//! trap rather than silently returning a wrong digest.
//!
//! ## What a broken or missing hook does
//!
//! It panics. A guest that reaches a container check with no working host has
//! no recoverable state, and a silent in-guest fallback would quietly defeat
//! the whole point of delegation - the caller would get correct bytes at a
//! speed the embedding was written to avoid. The message names the missing
//! wiring:
//!
//! ```text
//! lzma-turbo: no host hash hooks installed; the embedding program must call
//! lzma_turbo::hooks::install_host_hash_hooks before decoding
//! ```

use std::sync::RwLock;

/// An embedder-owned SHA-256 state.
///
/// Opaque to this crate, which only ever hands the value back to the hooks it
/// came from. The `u64` is public so an embedder can put whatever it likes in
/// it - a pointer, a slot index, an index-plus-generation - without this crate
/// needing to know.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HostSha256Handle(pub u64);

/// Resume a CRC-32/ISO-HDLC from `seed` (finalized domain) over `data`.
pub type Crc32Hook = fn(seed: u32, data: &[u8]) -> u32;

/// Resume a CRC-64/XZ from `seed` (finalized domain) over `data`.
pub type Crc64XzHook = fn(seed: u64, data: &[u8]) -> u64;

/// Create a fresh, live SHA-256 state.
pub type Sha256InitHook = fn() -> HostSha256Handle;

/// Duplicate a live SHA-256 state into a second, independent live one.
pub type Sha256CloneHook = fn(handle: HostSha256Handle) -> HostSha256Handle;

/// Append bytes to a live SHA-256 state.
pub type Sha256UpdateHook = fn(handle: HostSha256Handle, data: &[u8]);

/// Finish a live SHA-256 state, consuming the handle.
pub type Sha256FinalizeHook = fn(handle: HostSha256Handle) -> [u8; 32];

/// Release a live SHA-256 state without a digest.
pub type Sha256DropHook = fn(handle: HostSha256Handle);

/// The set of embedder-supplied delegation hooks.
///
/// They are plain `fn` pointers rather than trait objects or closures: they
/// carry no state, the set is `Copy`, and it can therefore be read on the hot
/// path without allocation. Any state the hooks need belongs to the embedding
/// program - which is precisely what [`HostSha256Handle`] names.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct HostHashHooks {
    /// Bulk CRC-32 (consumed when `crc-host` is enabled).
    pub crc32: Crc32Hook,
    /// Bulk CRC-64/XZ (consumed when `crc-host` is enabled).
    pub crc64_xz: Crc64XzHook,
    /// Open a SHA-256 state (consumed when `crypto-host` is enabled).
    pub sha256_init: Sha256InitHook,
    /// Duplicate a SHA-256 state.
    pub sha256_clone: Sha256CloneHook,
    /// Append to a SHA-256 state.
    pub sha256_update: Sha256UpdateHook,
    /// Finish a SHA-256 state and take its digest.
    pub sha256_finalize: Sha256FinalizeHook,
    /// Release a SHA-256 state without a digest.
    pub sha256_drop: Sha256DropHook,
}

impl HostHashHooks {
    /// A hook set, with every field required.
    ///
    /// The struct is `#[non_exhaustive]` so that a later primitive can be
    /// delegated without breaking existing embedders; this constructor is the
    /// way to build one from outside the crate today.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        crc32: Crc32Hook,
        crc64_xz: Crc64XzHook,
        sha256_init: Sha256InitHook,
        sha256_clone: Sha256CloneHook,
        sha256_update: Sha256UpdateHook,
        sha256_finalize: Sha256FinalizeHook,
        sha256_drop: Sha256DropHook,
    ) -> Self {
        Self {
            crc32,
            crc64_xz,
            sha256_init,
            sha256_clone,
            sha256_update,
            sha256_finalize,
            sha256_drop,
        }
    }
}

impl core::fmt::Debug for HostHashHooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("HostHashHooks { .. }")
    }
}

/// Process-wide hook registry.
///
/// A wasm guest is single-threaded and short-lived, so in the shipping
/// configuration this is written exactly once per instantiation. The lock keeps
/// the seam sound in native builds - where this crate's own tests are
/// multi-threaded - without any `unsafe`; a read guard per bulk chunk is
/// negligible next to the checksum the chunk represents.
static HOOKS: RwLock<Option<HostHashHooks>> = RwLock::new(None);

/// Install (or replace) the embedder's checksum hooks.
///
/// Call this before any decode that could verify a container check - in
/// practice, once at the top of the guest's entry point.
pub fn install_host_hash_hooks(hooks: HostHashHooks) {
    let mut slot = HOOKS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(hooks);
}

/// Whether hooks have been installed. Embedders can assert this in their own
/// start-up tests rather than discovering the gap inside a decode.
#[must_use]
pub fn host_hash_hooks_installed() -> bool {
    HOOKS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

/// Remove any installed hooks. Intended for embedder tests that need to prove
/// their wiring is what makes delegation work.
pub fn clear_host_hash_hooks() {
    let mut slot = HOOKS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
}

/// The installed hooks, or a panic naming the missing wiring.
///
/// Only a wasm build calls this outside `#[cfg(test)]`: on native targets the
/// in-process backends stay active (see [`crate::crc`] and
/// [`crate::crypto`]), so the delegating seams there are dead code by design.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn hooks() -> HostHashHooks {
    HOOKS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .expect(
            "lzma-turbo: no host hash hooks installed; the embedding program must call \
             lzma_turbo::hooks::install_host_hash_hooks before decoding",
        )
}

/// A reference hook set backed by this crate's own in-process primitives.
///
/// It is what a correct host does, so the tests can drive the real delegation
/// path - registry lookup, CRC seed chaining, SHA handle lifetime - on a native
/// target with no wasm runtime in sight. Every test installs this same set,
/// which keeps a parallel `cargo test` deterministic.
///
/// The CRCs come from `crc-fast` and the digest from whichever SHA-256 backend
/// this build has, exactly as the hashing rule requires: nothing here is a
/// hand-written checksum.
#[cfg(all(
    test,
    not(target_family = "wasm"),
    feature = "crc",
    any(feature = "crypto", feature = "native-crypto")
))]
pub(crate) fn install_reference_hooks_for_test() {
    /// The in-process SHA-256 the reference host hashes with. `native-crypto`
    /// wins when both are compiled, matching the crate's own precedence.
    #[cfg(feature = "native-crypto")]
    type RefSha = crate::crypto::rustcrypto::Sha256;
    #[cfg(all(feature = "crypto", not(feature = "native-crypto")))]
    type RefSha = crate::crypto::awslc::Sha256;

    // Both CRCs are "init all ones, final xor all ones", so `crc-fast`'s
    // running register is the bitwise complement of the finalized value in
    // both directions: seeding the digest with `!seed` resumes exactly where a
    // stream carrying the finalized `seed` left off.
    fn crc32(seed: u32, data: &[u8]) -> u32 {
        let mut digest = crc_fast::Digest::new_with_init_state(
            crc_fast::CrcAlgorithm::Crc32IsoHdlc,
            u64::from(!seed),
        );
        digest.update(data);
        digest.finalize() as u32
    }

    fn crc64_xz(seed: u64, data: &[u8]) -> u64 {
        let mut digest =
            crc_fast::Digest::new_with_init_state(crc_fast::CrcAlgorithm::Crc64Xz, !seed);
        digest.update(data);
        digest.finalize()
    }

    // The handle is a leaked `Box`, which is the simplest thing a host with a
    // real allocator does: `init`/`clone` leak one, `finalize`/`drop` take it
    // back. A handle used twice would be a use-after-free here, which is
    // exactly the contract violation the docs forbid.
    fn sha256_init() -> HostSha256Handle {
        HostSha256Handle(Box::into_raw(Box::new(RefSha::new())) as usize as u64)
    }

    fn state(handle: HostSha256Handle) -> *mut RefSha {
        handle.0 as usize as *mut RefSha
    }

    fn sha256_clone(handle: HostSha256Handle) -> HostSha256Handle {
        // SAFETY: `handle` is live by contract, so it is a pointer this
        // function family leaked and has not yet taken back; the clone is read
        // only and the original stays owned by the caller's handle.
        let copy = unsafe { (*state(handle)).clone() };
        HostSha256Handle(Box::into_raw(Box::new(copy)) as usize as u64)
    }

    fn sha256_update(handle: HostSha256Handle, data: &[u8]) {
        // SAFETY: as above; `handle` is live, and nothing else aliases it
        // because each handle is owned by exactly one guest-side hasher.
        unsafe { (*state(handle)).update(data) };
    }

    fn sha256_finalize(handle: HostSha256Handle) -> [u8; 32] {
        // SAFETY: `handle` is live and is consumed here, so taking the `Box`
        // back is the last use of the pointer.
        let boxed = unsafe { Box::from_raw(state(handle)) };
        boxed.finalize()
    }

    fn sha256_drop(handle: HostSha256Handle) {
        // SAFETY: as in `sha256_finalize` - the handle is live and consumed.
        drop(unsafe { Box::from_raw(state(handle)) });
    }

    install_host_hash_hooks(HostHashHooks::new(
        crc32,
        crc64_xz,
        sha256_init,
        sha256_clone,
        sha256_update,
        sha256_finalize,
        sha256_drop,
    ));
}

#[cfg(all(
    test,
    not(target_family = "wasm"),
    feature = "crc",
    any(feature = "crypto", feature = "native-crypto")
))]
mod tests {
    use super::*;

    /// The registry hands back exactly what was installed and reports its own
    /// state honestly - the two facts an embedder's wiring test depends on -
    /// and the reference hooks meet the published check vectors.
    #[test]
    fn installed_hooks_are_visible_and_dispatch() {
        install_reference_hooks_for_test();
        assert!(host_hash_hooks_installed());

        let hooks = hooks();
        // The check values both CRCs publish for the nine ASCII digits.
        assert_eq!((hooks.crc32)(0, b"123456789"), 0xcbf4_3926);
        assert_eq!((hooks.crc64_xz)(0, b"123456789"), 0x995d_c9bb_df19_39fa);

        // NIST's SHA-256 vector for "abc", through the whole handle lifetime.
        let handle = (hooks.sha256_init)();
        (hooks.sha256_update)(handle, b"abc");
        let digest = (hooks.sha256_finalize)(handle);
        assert_eq!(
            digest,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    /// The three CRC seed properties the contract states, at both widths:
    /// zero seeds the stream, an empty update is the identity, and chaining a
    /// split equals the whole stream.
    #[test]
    fn crc_hooks_satisfy_the_seed_contract() {
        install_reference_hooks_for_test();
        let hooks = hooks();

        let data: Vec<u8> = (0u32..4096).map(|i| (i * 37 + 11) as u8).collect();

        // 1. Zero seeds the stream.
        assert_eq!((hooks.crc32)(0, &data), crate::crc::crc32(&data));
        assert_eq!((hooks.crc64_xz)(0, &data), crate::crc::crc64_xz(&data));
        assert_eq!((hooks.crc32)(0, &[]), 0);
        assert_eq!((hooks.crc64_xz)(0, &[]), 0);

        // 2. An empty update is the identity, for an arbitrary seed.
        assert_eq!((hooks.crc32)(0x1234_5678, &[]), 0x1234_5678);
        assert_eq!(
            (hooks.crc64_xz)(0x0123_4567_89ab_cdef, &[]),
            0x0123_4567_89ab_cdef
        );

        // 3. Chaining a split equals the whole stream, at every cut.
        for cut in [0usize, 1, 2, 17, 64, 1000, 4095, 4096] {
            let (a, b) = data.split_at(cut);
            assert_eq!(
                (hooks.crc32)((hooks.crc32)(0, a), b),
                crate::crc::crc32(&data),
                "crc32 chain, cut {cut}"
            );
            assert_eq!(
                (hooks.crc64_xz)((hooks.crc64_xz)(0, a), b),
                crate::crc::crc64_xz(&data),
                "crc64 chain, cut {cut}"
            );
        }
    }

    /// A cloned SHA-256 handle is independent: feeding one must not move the
    /// other, and dropping a handle instead of finalizing it must be a legal
    /// end for it.
    #[test]
    fn sha256_handles_clone_independently_and_may_be_dropped() {
        install_reference_hooks_for_test();
        let hooks = hooks();

        let a = (hooks.sha256_init)();
        (hooks.sha256_update)(a, b"abc");
        let b = (hooks.sha256_clone)(a);
        (hooks.sha256_update)(a, b"def");

        // `b` still hashes only "abc"; `a` has moved on to "abcdef".
        let mut want_ab = crate::crypto::Sha256::new();
        want_ab.update(b"abc");
        assert_eq!((hooks.sha256_finalize)(b), want_ab.finalize());

        let mut want_abcdef = crate::crypto::Sha256::new();
        want_abcdef.update(b"abcdef");
        assert_eq!((hooks.sha256_finalize)(a), want_abcdef.finalize());

        // A handle that never gets a digest is released, not leaked.
        let unused = (hooks.sha256_init)();
        (hooks.sha256_update)(unused, b"nothing will read this");
        (hooks.sha256_drop)(unused);
    }
}
