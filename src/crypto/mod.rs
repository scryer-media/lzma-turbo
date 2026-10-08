//! SHA-256, as the xz container uses it.
//!
//! C: `C/Sha256.c`. As with [`crate::crc`] none of this is reachable from the
//! decoder; it is what a container reader needs to verify an xz stream whose
//! check type is SHA-256.
//!
//! There are three backends behind one API:
//!
//! - `awslc`, from `aws-lc-rs`, behind the default `crypto` feature;
//! - `rustcrypto`, from the `sha2` crate, behind `native-crypto`, for a
//!   build that wants no C toolchain;
//! - `host`, behind `crypto-host`, which hands the hashing to the embedding
//!   program through [`crate::hooks`] - for a wasm guest, where there are no
//!   SHA instructions to reach and the host has them.
//!
//! The features are additive, and the selection is by precedence:
//!
//! 1. **`host`**, on `wasm32` with `crypto-host`. Only there: on a native
//!    target `crypto-host` is accepted and inert, so feature unification in a
//!    mixed workspace cannot silently turn a native build into a delegating
//!    one.
//! 2. **`rustcrypto`**, with `native-crypto`. It wins over `crypto`
//!    deliberately - the opt-out has to be an opt-out even when something else
//!    in the dependency graph turns `crypto` back on. `crypto-host` implies
//!    it, so a `crypto-host` build always has an in-process backend off wasm.
//! 3. **`awslc`**, with `crypto`.
//!
//! Every enabled backend is still *compiled*, whether or not it is selected:
//! with `crypto` and `native-crypto` both on, a test requires the two to agree
//! digest for digest, and the `host` module carries a native test of its
//! handle lifetime against the reference hooks.

#[cfg(feature = "crypto")]
pub mod awslc;
// Compiled on native targets as well as wasm, where it is not the active
// backend and its seam is therefore dead code - which is what lets its handle
// lifetime be tested without a wasm runtime.
#[cfg(feature = "crypto-host")]
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "crypto-host")),
    allow(dead_code)
)]
pub mod host;
#[cfg(feature = "native-crypto")]
pub mod rustcrypto;

/// Whether [`Sha256`] hashes in this process or hands every chunk to the
/// embedding program (see [`crate::hooks`]). Only a `wasm32` build with
/// `crypto-host` delegates.
pub const SHA256_IS_HOST_DELEGATED: bool =
    cfg!(all(target_arch = "wasm32", feature = "crypto-host"));

/// Which backend [`Sha256`] is: `"aws-lc-rs"`, `"rustcrypto"` (the `sha2`
/// crate) or `"host"`. Chosen by the same features as the re-export below,
/// so a program reporting its build can say which one it measured.
pub const SHA256_BACKEND: &str = if SHA256_IS_HOST_DELEGATED {
    "host"
} else if cfg!(feature = "native-crypto") {
    "rustcrypto"
} else {
    "aws-lc-rs"
};

#[cfg(all(
    feature = "crypto",
    not(feature = "native-crypto"),
    not(all(target_arch = "wasm32", feature = "crypto-host"))
))]
pub use awslc::Sha256;
#[cfg(all(target_arch = "wasm32", feature = "crypto-host"))]
pub use host::Sha256;
#[cfg(all(
    feature = "native-crypto",
    not(all(target_arch = "wasm32", feature = "crypto-host"))
))]
pub use rustcrypto::Sha256;

/// What can go wrong in this module. Nothing here is a decode error, so it
/// is a type of its own rather than a variant of [`crate::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    /// The backend refused the operation.
    Backend,
}

impl core::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Backend => "the cryptographic backend refused the operation",
        })
    }
}

impl core::error::Error for CryptoError {}

/// Length of a SHA-256 digest, in bytes.
pub const SHA256_LEN: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;

    /// The name says which module `Sha256` is re-exported from.
    #[test]
    fn the_backend_name_is_the_selected_backend() {
        let module = match SHA256_BACKEND {
            "aws-lc-rs" => "::awslc::",
            "rustcrypto" => "::rustcrypto::",
            "host" => "::host::",
            other => panic!("unknown backend name {other:?}"),
        };
        let ty = core::any::type_name::<Sha256>();
        assert!(ty.contains(module), "{SHA256_BACKEND} names {ty}");
    }

    /// NIST's SHA-256 short-message vectors, plus the empty string.
    #[test]
    fn sha256_known_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (input, want) in cases {
            let mut h = Sha256::new();
            h.update(input);
            assert_eq!(hex(&h.finalize()), *want);
        }
    }

    /// A million 'a's: the vector that catches a broken length or padding
    /// block, and the only one that exercises the multi-buffer path.
    #[test]
    fn sha256_million_a() {
        let mut h = Sha256::new();
        let block = [b'a'; 1000];
        for _ in 0..1000 {
            h.update(&block);
        }
        assert_eq!(
            hex(&h.finalize()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// With both backends compiled they must agree, byte for byte. This is
    /// the same differential discipline the decoder uses.
    #[cfg(all(feature = "crypto", feature = "native-crypto"))]
    #[test]
    fn the_two_backends_agree() {
        let data: alloc::vec::Vec<u8> = (0u32..5000).map(|i| (i * 31 + 7) as u8).collect();
        for n in [0usize, 1, 55, 56, 64, 1000, 5000] {
            let mut a = rustcrypto::Sha256::new();
            let mut b = awslc::Sha256::new();
            a.update(&data[..n]);
            b.update(&data[..n]);
            assert_eq!(a.finalize(), b.finalize(), "sha256 over {n} bytes");
        }
    }

    fn hex(bytes: &[u8]) -> alloc::string::String {
        use core::fmt::Write as _;
        let mut s = alloc::string::String::new();
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}
