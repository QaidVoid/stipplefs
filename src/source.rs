//! Access to the bytes an image is stored in.
//!
//! An image occupies a range of some larger byte source, so the reader is
//! built from a source together with that range rather than from a path.

use crate::error::{Error, Result};

/// A source of bytes that an image can be read from.
pub trait ByteSource {
    /// Total number of bytes available from this source.
    fn size(&self) -> u64;

    /// Fill `buf` with the bytes starting at `offset`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Borrow the whole source when its bytes are already contiguous in
    /// memory.
    ///
    /// Sources that cannot hand out a borrow, such as one backed by a file,
    /// return `None`. Callers must behave correctly either way; this exists
    /// so that metadata can be searched in place instead of being copied.
    fn as_slice(&self) -> Option<&[u8]> {
        None
    }
}

fn slice_read(bytes: &[u8], offset: u64, buf: &mut [u8]) -> Result<()> {
    let available = bytes.len() as u64;
    let end = offset
        .checked_add(buf.len() as u64)
        .ok_or(Error::OutOfBounds {
            offset,
            len: buf.len() as u64,
            available,
        })?;
    if end > available {
        return Err(Error::OutOfBounds {
            offset,
            len: buf.len() as u64,
            available,
        });
    }
    buf.copy_from_slice(&bytes[offset as usize..end as usize]);
    Ok(())
}

impl ByteSource for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        slice_read(self, offset, buf)
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(self)
    }
}

impl ByteSource for Vec<u8> {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        slice_read(self, offset, buf)
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(self)
    }
}

/// A byte source narrowed to the range one image occupies within it.
///
/// Offsets passed to a region are relative to the start of the image, which
/// is what every offset stored inside an image is relative to.
pub struct Region<'a> {
    source: Box<dyn ByteSource + 'a>,
    start: u64,
    size: u64,
}

impl<'a> Region<'a> {
    /// Narrow `source` to `size` bytes beginning at `start`.
    ///
    /// Fails when the requested range is not entirely inside the source.
    pub fn new(source: impl ByteSource + 'a, start: u64, size: u64) -> Result<Self> {
        let available = source.size();
        let end = start.checked_add(size).ok_or(Error::OutOfBounds {
            offset: start,
            len: size,
            available,
        })?;
        if end > available {
            return Err(Error::OutOfBounds {
                offset: start,
                len: size,
                available,
            });
        }
        Ok(Region {
            source: Box::new(source),
            start,
            size,
        })
    }

    /// Number of bytes the image occupies.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Fill `buf` with image bytes starting at `offset`.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                available: self.size,
            })?;
        if end > self.size {
            return Err(Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                available: self.size,
            });
        }
        self.source.read_at(self.start + offset, buf)
    }

    /// Borrow image bytes directly when the underlying source allows it.
    pub fn slice(&self, offset: u64, len: u64) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        if end > self.size {
            return None;
        }
        let bytes = self.source.as_slice()?;
        let from = (self.start + offset) as usize;
        bytes.get(from..from + len as usize)
    }
}

/// A byte source backed by an open file.
pub struct FileSource {
    file: std::fs::File,
    size: u64,
}

impl FileSource {
    /// Wrap an already-open file.
    pub fn new(file: std::fs::File) -> Result<Self> {
        let size = file.metadata()?.len();
        Ok(FileSource { file, size })
    }
}

impl ByteSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                available: self.size,
            })?;
        if end > self.size {
            return Err(Error::OutOfBounds {
                offset,
                len: buf.len() as u64,
                available: self.size,
            });
        }
        self.file.read_exact_at(buf, offset)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<u8> {
        (0..4096u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn in_memory_reads_at_a_non_zero_offset() {
        let data = corpus();
        let mut buf = [0u8; 16];
        data.read_at(1000, &mut buf).unwrap();
        assert_eq!(buf[..], data[1000..1016]);
    }

    #[test]
    fn region_offsets_are_image_relative() {
        let data = corpus();
        let region = Region::new(data.clone(), 1000, 500).unwrap();
        assert_eq!(region.size(), 500);
        let mut buf = [0u8; 16];
        region.read_at(0, &mut buf).unwrap();
        assert_eq!(buf[..], data[1000..1016]);
        region.read_at(484, &mut buf).unwrap();
        assert_eq!(buf[..], data[1484..1500]);
    }

    #[test]
    fn region_rejects_reads_past_its_end() {
        let region = Region::new(corpus(), 1000, 500).unwrap();
        let mut buf = [0u8; 16];
        assert!(region.read_at(485, &mut buf).is_err());
        assert!(region.read_at(u64::MAX, &mut buf).is_err());
    }

    #[test]
    fn region_rejects_a_range_outside_the_source() {
        assert!(Region::new(corpus(), 4000, 500).is_err());
        assert!(Region::new(corpus(), u64::MAX, 1).is_err());
    }

    #[test]
    fn in_memory_source_lends_a_borrowed_slice() {
        let data = corpus();
        let region = Region::new(data.clone(), 1000, 500).unwrap();
        assert_eq!(region.slice(0, 16).unwrap(), &data[1000..1016]);
        assert!(region.slice(490, 16).is_none());
    }

    #[test]
    fn file_and_memory_sources_agree() {
        let data = corpus();
        let path = std::env::temp_dir().join("stipplefs-source-test.bin");
        std::fs::write(&path, &data).unwrap();
        let file = FileSource::new(std::fs::File::open(&path).unwrap()).unwrap();

        assert_eq!(file.size(), data.len() as u64);
        assert!(file.as_slice().is_none());

        for off in [0u64, 1, 1000, 4080] {
            let mut a = [0u8; 16];
            let mut b = [0u8; 16];
            file.read_at(off, &mut a).unwrap();
            data.read_at(off, &mut b).unwrap();
            assert_eq!(a, b, "mismatch at offset {off}");
        }
        assert!(file.read_at(4090, &mut [0u8; 16]).is_err());
        std::fs::remove_file(&path).ok();
    }
}
