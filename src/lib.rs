//! A read-only, compressed, deduplicating filesystem image format.
//!
//! The crate is split into two independent halves selected by Cargo features.
//! The `read` feature provides everything needed to open an image and serve
//! byte ranges from it. The `write` feature provides image construction. A
//! build may enable either, both, or neither.

#![warn(missing_docs)]

pub mod error;
pub mod format;

#[cfg(feature = "read")]
pub mod cache;

#[cfg(feature = "read")]
pub mod read;

#[cfg(feature = "read")]
pub mod source;
