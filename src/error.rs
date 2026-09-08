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
    /// The image does not begin with the expected magic bytes.
    BadMagic,
    /// The image uses a major format version this build does not implement.
    UnsupportedVersion {
        /// Major version found in the image.
        major: u16,
        /// Major version this build implements.
        supported: u16,
    },
    /// The image predates a layout change this build cannot interpret.
    UnsupportedMinorVersion {
        /// Minor version found in the image.
        minor: u16,
        /// Lowest minor version this build reads.
        supported: u16,
    },
    /// The source tree holds something the format cannot record.
    Unrepresentable {
        /// Where it was found.
        path: String,
        /// What was wrong with it.
        detail: &'static str,
    },
    /// Temporary space for building an image could not be obtained.
    Temporary {
        /// Directory the writer tried to use.
        directory: String,
        /// What the operating system reported.
        detail: String,
    },
    /// A stored digest did not match the bytes it covers.
    ChecksumMismatch {
        /// What the digest covers.
        what: &'static str,
    },
    /// A recorded field holds a value the format does not permit.
    Malformed {
        /// What is wrong.
        detail: &'static str,
    },
    /// The image uses a codec this build cannot decode.
    UnsupportedCodec {
        /// Bitmask of the codecs the image needs but this build lacks.
        missing: u16,
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
            Error::BadMagic => write!(f, "not a stipplefs image: magic bytes do not match"),
            Error::UnsupportedVersion { major, supported } => write!(
                f,
                "image format version {major} is not supported by this build, which implements version {supported}"
            ),
            Error::UnsupportedMinorVersion { minor, supported } => write!(
                f,
                "image format version 1.{minor} predates a layout change; this build reads 1.{supported} and later"
            ),
            Error::Unrepresentable { path, detail } => {
                write!(f, "{path}: {detail}")
            }
            Error::Temporary { directory, detail } => write!(
                f,
                "could not obtain temporary space in {directory}: {detail}; \
                 set TMPDIR to a writable directory with room for the tree"
            ),
            Error::ChecksumMismatch { what } => write!(f, "{what} failed its integrity check"),
            Error::Malformed { detail } => write!(f, "malformed image: {detail}"),
            Error::UnsupportedCodec { missing } => write!(
                f,
                "image uses codecs this build cannot decode (missing mask {missing:#06x})"
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
