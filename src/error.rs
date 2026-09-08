//! Errors produced when reading or writing an image.

use std::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong reading or writing an image.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A read extended past the end of the byte source.
    OutOfBounds {
        /// Offset the read started at.
        offset: u64,
        /// Number of bytes requested.
        len: u64,
        /// Number of bytes the source holds.
        available: u64,
    },
    /// The underlying byte source failed.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfBounds {
                offset,
                len,
                available,
            } => write!(
                f,
                "read of {len} bytes at offset {offset} extends past the end of a {available} byte source"
            ),
            Error::Io(e) => write!(f, "byte source failed: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
