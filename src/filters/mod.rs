//! The in-place converters that sit in front of an LZMA2 stream: the
//! branch/call/jump family ([`bcj`]) and the delta filter ([`delta`]).
//!
//! They are defined by the xz file format (spec §5.3.2 and §5.3.3) and ported
//! from the same public-domain C the rest of this crate comes from, but they
//! are not xz's alone: 7z carries the same filters under the same ids, so a 7z
//! reader needs them without the `.xz` stream layer, its readers or its
//! checksums. That is what the `filters` feature is: this module, on its own,
//! `no_std` and with no dependency. The `xz` feature implies it and re-exports
//! both modules at `xz::bcj` and `xz::delta`, where they have always been.
//!
//! Each converter works in place, returns how many leading bytes it converted,
//! and leaves the tail to the caller; see the module docs for the contract.

pub mod bcj;
pub mod bcj2;
pub mod delta;
