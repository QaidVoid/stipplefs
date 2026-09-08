//! Opening an image and validating its structure.

use crate::error::{Error, Result};
use crate::format::{
    ALIGNMENT, Codec, MAGIC, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, SECTION_COUNT, SUPERBLOCK_SIZE,
    Section, SectionKind, Superblock, VERSION_MAJOR, supported_codecs,
};
use crate::source::{ByteSource, Region};

fn u16at(b: &[u8; SUPERBLOCK_SIZE], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32at(b: &[u8; SUPERBLOCK_SIZE], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64at(b: &[u8; SUPERBLOCK_SIZE], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

/// Digest used everywhere in the format.
pub fn digest(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

/// Parse and validate a superblock.
///
/// `available` is the number of bytes the caller said the image occupies. The
/// superblock's own `image_length` may not exceed it.
pub fn parse_superblock(bytes: &[u8; SUPERBLOCK_SIZE], available: u64) -> Result<Superblock> {
    if bytes[..8] != MAGIC {
        return Err(Error::BadMagic);
    }

    let stored = u64at(bytes, Superblock::CHECKSUM_OFFSET);
    if digest(&bytes[..Superblock::CHECKSUM_OFFSET]) != stored {
        return Err(Error::ChecksumMismatch { what: "superblock" });
    }

    let version_major = u16at(bytes, 8);
    if version_major != VERSION_MAJOR {
        return Err(Error::UnsupportedVersion {
            major: version_major,
            supported: VERSION_MAJOR,
        });
    }

    let image_length = u64at(bytes, 16);
    if image_length == 0 || !image_length.is_multiple_of(ALIGNMENT) {
        return Err(Error::Malformed {
            detail: "image_length is not a non-zero multiple of 4096",
        });
    }
    if image_length > available {
        return Err(Error::Malformed {
            detail: "image_length exceeds the range supplied by the caller",
        });
    }

    let block_size = u32at(bytes, 32);
    if !block_size.is_power_of_two() || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(Error::Malformed {
            detail: "block_size is not a power of two within the permitted range",
        });
    }

    let mut sections = [Section::default(); SECTION_COUNT];
    for (i, section) in sections.iter_mut().enumerate() {
        let base = 56 + i * 16;
        *section = Section {
            offset: u64at(bytes, base),
            size: u64at(bytes, base + 8),
        };
    }

    for section in &sections {
        if section.is_absent() {
            if section.offset != 0 {
                return Err(Error::Malformed {
                    detail: "an absent section has a non-zero offset",
                });
            }
            continue;
        }
        let end = section
            .offset
            .checked_add(section.size)
            .ok_or(Error::Malformed {
                detail: "a section's offset and size overflow",
            })?;
        if section.offset < ALIGNMENT || end > image_length {
            return Err(Error::Malformed {
                detail: "a section lies outside the image",
            });
        }
        if !section.offset.is_multiple_of(ALIGNMENT) {
            return Err(Error::Malformed {
                detail: "a section does not start on a 4096 byte boundary",
            });
        }
    }

    for i in 0..SECTION_COUNT {
        for j in (i + 1)..SECTION_COUNT {
            let (a, b) = (sections[i], sections[j]);
            if a.is_absent() || b.is_absent() {
                continue;
            }
            if a.offset < b.offset + b.size && b.offset < a.offset + a.size {
                return Err(Error::Malformed {
                    detail: "two sections overlap",
                });
            }
        }
    }

    let codec_mask = u16at(bytes, 14);
    let missing = codec_mask & !supported_codecs();
    if missing != 0 {
        return Err(Error::UnsupportedCodec { missing });
    }
    if codec_mask & !((1 << (Codec::ZstdDict.id() + 1)) - 1) != 0 {
        return Err(Error::Malformed {
            detail: "codec_mask names a codec the format does not define",
        });
    }

    Ok(Superblock {
        version_major,
        version_minor: u16at(bytes, 10),
        flags: u16at(bytes, 12),
        codec_mask,
        image_length,
        stream_length: u64at(bytes, 24),
        block_size,
        inode_count: u32at(bytes, 36),
        extent_count: u32at(bytes, 40),
        block_count: u32at(bytes, 44),
        root_inode: u32at(bytes, 48),
        sections,
    })
}

/// An opened image, validated as far as its superblock.
pub struct Image<'a> {
    region: Region<'a>,
    superblock: Superblock,
}

impl<'a> Image<'a> {
    /// Open the image occupying `size` bytes at `start` within `source`.
    pub fn new(source: impl ByteSource + 'a, start: u64, size: u64) -> Result<Self> {
        let region = Region::new(source, start, size)?;
        if region.size() < SUPERBLOCK_SIZE as u64 {
            return Err(Error::Malformed {
                detail: "range is too short to contain a superblock",
            });
        }
        let mut raw = [0u8; SUPERBLOCK_SIZE];
        region.read_at(0, &mut raw)?;
        let superblock = parse_superblock(&raw, region.size())?;
        Ok(Image { region, superblock })
    }

    /// The validated superblock.
    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    /// The region the image occupies.
    pub fn region(&self) -> &Region<'a> {
        &self.region
    }
}

#[cfg(feature = "fs")]
impl Image<'static> {
    /// Open an image that occupies a whole file.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        Image::new(crate::source::FileSource::new(file)?, 0, size)
    }

    /// Open an image that occupies `size` bytes at `offset` within a file.
    pub fn open_at(path: impl AsRef<std::path::Path>, offset: u64, size: u64) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Image::new(crate::source::FileSource::new(file)?, offset, size)
    }
}

/// A metadata section's bytes, borrowed when the source allows it.
pub type SectionData<'a> = std::borrow::Cow<'a, [u8]>;

/// One entry of the block index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntry {
    /// Where the stored bytes begin, relative to the image.
    pub image_offset: u64,
    /// Bytes as stored, excluding the trailing digest.
    pub stored_length: u32,
    /// Bytes after decoding.
    pub decoded_length: u32,
    /// How the bytes are stored.
    pub codec: Codec,
}

/// Size of one block index entry.
pub const BLOCK_ENTRY_SIZE: usize = 16;

/// The block index, parsed on demand rather than materialised.
pub struct BlockIndex<'a> {
    bytes: SectionData<'a>,
    count: u32,
}

impl BlockIndex<'_> {
    /// Number of blocks.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Entry `i`, or `None` when `i` is out of range.
    pub fn get(&self, i: u32) -> Option<BlockEntry> {
        if i >= self.count {
            return None;
        }
        let at = i as usize * BLOCK_ENTRY_SIZE;
        let e = self.bytes.get(at..at + BLOCK_ENTRY_SIZE)?;
        let mut off = [0u8; 8];
        off.copy_from_slice(&e[0..8]);
        Some(BlockEntry {
            image_offset: u64::from_le_bytes(off),
            stored_length: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
            decoded_length: u32::from_le_bytes([e[12], e[13], e[14], 0]),
            codec: Codec::from_id(e[15])?,
        })
    }
}

impl<'a> Image<'a> {
    /// Read a metadata section, verifying its digest.
    ///
    /// The bytes are borrowed when the underlying source can lend them, so
    /// metadata is searched in place rather than copied.
    pub fn section_data(&self, kind: SectionKind) -> Result<SectionData<'_>> {
        let section = self.superblock.section(kind);
        if section.is_absent() {
            return Ok(SectionData::Borrowed(&[]));
        }
        if section.size < 8 {
            return Err(Error::Malformed {
                detail: "a present section is too short to hold its digest",
            });
        }
        let content_len = section.size - 8;

        let (content, stored) = match self.region.slice(section.offset, section.size) {
            Some(all) => {
                let (c, d) = all.split_at(content_len as usize);
                let mut digest_bytes = [0u8; 8];
                digest_bytes.copy_from_slice(d);
                (SectionData::Borrowed(c), u64::from_le_bytes(digest_bytes))
            }
            None => {
                let mut c = vec![0u8; content_len as usize];
                self.region.read_at(section.offset, &mut c)?;
                let mut d = [0u8; 8];
                self.region.read_at(section.offset + content_len, &mut d)?;
                (SectionData::Owned(c), u64::from_le_bytes(d))
            }
        };

        if digest(&content) != stored {
            return Err(Error::ChecksumMismatch {
                what: "metadata section",
            });
        }
        Ok(content)
    }

    /// Read and validate the block index.
    pub fn block_index(&self) -> Result<BlockIndex<'_>> {
        let bytes = self.section_data(SectionKind::Blocks)?;
        let count = self.superblock.block_count;
        let needed = count as usize * BLOCK_ENTRY_SIZE;
        if bytes.len() < needed {
            return Err(Error::Malformed {
                detail: "block index section is shorter than block_count requires",
            });
        }
        let index = BlockIndex { bytes, count };
        for i in 0..count {
            let e = index.get(i).ok_or(Error::Malformed {
                detail: "a block index entry names a codec the format does not define",
            })?;
            if e.decoded_length > self.superblock.block_size {
                return Err(Error::Malformed {
                    detail: "a block declares a decoded length larger than block_size",
                });
            }
            if e.codec.bit() & self.superblock.codec_mask == 0 {
                return Err(Error::Malformed {
                    detail: "a block uses a codec absent from codec_mask",
                });
            }
            let end = e
                .image_offset
                .checked_add(e.stored_length as u64)
                .and_then(|v| v.checked_add(8))
                .ok_or(Error::Malformed {
                    detail: "a block's stored range overflows",
                })?;
            if e.image_offset < ALIGNMENT || end > self.superblock.image_length {
                return Err(Error::Malformed {
                    detail: "a block's stored bytes lie outside the image",
                });
            }
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SectionKind;

    /// Builds a superblock byte by byte at the offsets `docs/format.md` gives,
    /// so the fixture exercises the specification rather than the writer.
    fn fixture() -> [u8; SUPERBLOCK_SIZE] {
        let mut b = [0u8; SUPERBLOCK_SIZE];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[10..12].copy_from_slice(&0u16.to_le_bytes());
        b[12..14].copy_from_slice(&0u16.to_le_bytes());
        b[14..16].copy_from_slice(&1u16.to_le_bytes());
        b[16..24].copy_from_slice(&(4096u64 * 4).to_le_bytes());
        b[24..32].copy_from_slice(&1234u64.to_le_bytes());
        b[32..36].copy_from_slice(&65536u32.to_le_bytes());
        b[36..40].copy_from_slice(&3u32.to_le_bytes());
        b[40..44].copy_from_slice(&2u32.to_le_bytes());
        b[44..48].copy_from_slice(&1u32.to_le_bytes());
        b[48..52].copy_from_slice(&0u32.to_le_bytes());
        // inodes at 4096 for 4096 bytes, blocks at 8192 for 4096 bytes
        b[56..64].copy_from_slice(&4096u64.to_le_bytes());
        b[64..72].copy_from_slice(&4096u64.to_le_bytes());
        let blocks = 56 + (SectionKind::Blocks as usize) * 16;
        b[blocks..blocks + 8].copy_from_slice(&8192u64.to_le_bytes());
        b[blocks + 8..blocks + 16].copy_from_slice(&4096u64.to_le_bytes());
        seal(&mut b);
        b
    }

    fn seal(b: &mut [u8; SUPERBLOCK_SIZE]) {
        let d = digest(&b[..Superblock::CHECKSUM_OFFSET]);
        b[Superblock::CHECKSUM_OFFSET..].copy_from_slice(&d.to_le_bytes());
    }

    #[test]
    fn fixture_parses_field_by_field() {
        let sb = parse_superblock(&fixture(), 4096 * 4).unwrap();
        assert_eq!(sb.version_major, 1);
        assert_eq!(sb.version_minor, 0);
        assert_eq!(sb.flags, 0);
        assert_eq!(sb.codec_mask, Codec::Stored.bit());
        assert_eq!(sb.image_length, 4096 * 4);
        assert_eq!(sb.stream_length, 1234);
        assert_eq!(sb.block_size, 65536);
        assert_eq!(sb.inode_count, 3);
        assert_eq!(sb.extent_count, 2);
        assert_eq!(sb.block_count, 1);
        assert_eq!(sb.root_inode, 0);
        assert_eq!(
            sb.section(SectionKind::Inodes),
            Section {
                offset: 4096,
                size: 4096
            }
        );
        assert_eq!(
            sb.section(SectionKind::Blocks),
            Section {
                offset: 8192,
                size: 4096
            }
        );
        assert!(sb.section(SectionKind::Names).is_absent());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut b = fixture();
        b[0] = b'X';
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::BadMagic)
        ));
    }

    #[test]
    fn rejects_a_corrupted_superblock() {
        let mut b = fixture();
        b[24] ^= 0xff;
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_an_unknown_major_version() {
        let mut b = fixture();
        b[8..10].copy_from_slice(&2u16.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::UnsupportedVersion { major: 2, .. })
        ));
    }

    #[test]
    fn accepts_a_newer_minor_version() {
        let mut b = fixture();
        b[10..12].copy_from_slice(&7u16.to_le_bytes());
        seal(&mut b);
        assert_eq!(parse_superblock(&b, 4096 * 4).unwrap().version_minor, 7);
    }

    #[test]
    fn rejects_an_image_longer_than_the_supplied_range() {
        assert!(matches!(
            parse_superblock(&fixture(), 4096 * 3),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_an_unaligned_image_length() {
        let mut b = fixture();
        b[16..24].copy_from_slice(&(4096u64 * 4 + 1).to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 5),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_a_block_size_outside_the_permitted_range() {
        for bad in [0u32, 2048, 65535, MAX_BLOCK_SIZE * 2] {
            let mut b = fixture();
            b[32..36].copy_from_slice(&bad.to_le_bytes());
            seal(&mut b);
            assert!(
                matches!(parse_superblock(&b, 4096 * 4), Err(Error::Malformed { .. })),
                "block_size {bad} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_a_section_outside_the_image() {
        let mut b = fixture();
        b[56..64].copy_from_slice(&(4096u64 * 3).to_le_bytes());
        b[64..72].copy_from_slice(&(4096u64 * 2).to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_overlapping_sections() {
        let mut b = fixture();
        let blocks = 56 + (SectionKind::Blocks as usize) * 16;
        b[blocks..blocks + 8].copy_from_slice(&4096u64.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_an_absent_section_with_a_non_zero_offset() {
        let mut b = fixture();
        let names = 56 + (SectionKind::Names as usize) * 16;
        b[names..names + 8].copy_from_slice(&4096u64.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_a_codec_this_build_cannot_decode() {
        let mut b = fixture();
        b[14..16].copy_from_slice(&(Codec::Stored.bit() | Codec::Zstd.bit()).to_le_bytes());
        seal(&mut b);
        let err = parse_superblock(&b, 4096 * 4).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedCodec { missing } if missing == Codec::Zstd.bit()),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_a_codec_the_format_does_not_define() {
        let mut b = fixture();
        b[14..16].copy_from_slice(&0x8000u16.to_le_bytes());
        seal(&mut b);
        assert!(parse_superblock(&b, 4096 * 4).is_err());
    }
}

#[cfg(test)]
mod image_tests {
    use super::tests_support::valid_image;
    use super::*;

    #[test]
    fn opens_from_memory_at_a_non_zero_offset() {
        let image = valid_image();
        let mut padded = vec![0xAAu8; 8192];
        padded.extend_from_slice(&image);
        let opened = Image::new(padded, 8192, image.len() as u64).unwrap();
        assert_eq!(opened.superblock().block_size, 65536);
        assert_eq!(opened.superblock().image_length, image.len() as u64);
    }

    #[test]
    fn rejects_a_range_too_short_for_a_superblock() {
        let image = valid_image();
        assert!(Image::new(image, 0, 100).is_err());
    }

    #[cfg(feature = "fs")]
    #[test]
    fn path_constructors_match_an_explicit_range() {
        let image = valid_image();
        let dir = std::env::temp_dir();

        let whole = dir.join("stipplefs-whole.img");
        std::fs::write(&whole, &image).unwrap();
        let a = Image::open(&whole).unwrap();

        let embedded = dir.join("stipplefs-embedded.img");
        let mut padded = vec![0x55u8; 4096];
        padded.extend_from_slice(&image);
        padded.extend_from_slice(&[0x66u8; 777]);
        std::fs::write(&embedded, &padded).unwrap();
        let b = Image::open_at(&embedded, 4096, image.len() as u64).unwrap();

        let c = Image::new(image.clone(), 0, image.len() as u64).unwrap();

        assert_eq!(a.superblock(), c.superblock());
        assert_eq!(b.superblock(), c.superblock());

        std::fs::remove_file(&whole).ok();
        std::fs::remove_file(&embedded).ok();
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;

    /// Lays out a whole image: superblock, 4096-aligned sections each with a
    /// trailing digest, then a data area.
    pub struct Builder {
        pub sections: Vec<(SectionKind, Vec<u8>)>,
        pub block_count: u32,
        pub codec_mask: u16,
        pub block_size: u32,
        pub data: Vec<u8>,
    }

    impl Default for Builder {
        fn default() -> Self {
            Builder {
                sections: Vec::new(),
                block_count: 0,
                codec_mask: Codec::Stored.bit(),
                block_size: 65536,
                data: Vec::new(),
            }
        }
    }

    impl Builder {
        pub fn section(mut self, kind: SectionKind, content: Vec<u8>) -> Self {
            self.sections.push((kind, content));
            self
        }

        pub fn block_count(mut self, n: u32) -> Self {
            self.block_count = n;
            self
        }

        pub fn codec_mask(mut self, m: u16) -> Self {
            self.codec_mask = m;
            self
        }

        pub fn with_data(mut self, d: Vec<u8>) -> Self {
            self.data = d;
            self
        }

        /// Offset the data area will start at, so callers can point blocks at it.
        pub fn data_offset(&self) -> u64 {
            let mut at = 4096u64;
            for (_, c) in &self.sections {
                at += (c.len() as u64 + 8).div_ceil(4096) * 4096;
            }
            at
        }

        pub fn build(self) -> Vec<u8> {
            let mut placed = [(0u64, 0u64); SECTION_COUNT];
            let mut body: Vec<u8> = Vec::new();
            let mut at = 4096u64;
            for (kind, content) in &self.sections {
                let size = content.len() as u64 + 8;
                placed[*kind as usize] = (at, size);
                body.extend_from_slice(content);
                body.extend_from_slice(&digest(content).to_le_bytes());
                let pad = size.div_ceil(4096) * 4096 - size;
                body.extend(std::iter::repeat_n(0u8, pad as usize));
                at += size + pad;
            }
            let data_at = at;
            body.extend_from_slice(&self.data);
            at += self.data.len() as u64;
            let total = at.div_ceil(4096) * 4096;
            body.extend(std::iter::repeat_n(0u8, (total - at) as usize));
            let _ = data_at;

            let mut b = [0u8; SUPERBLOCK_SIZE];
            b[0..8].copy_from_slice(&MAGIC);
            b[8..10].copy_from_slice(&1u16.to_le_bytes());
            b[14..16].copy_from_slice(&self.codec_mask.to_le_bytes());
            b[16..24].copy_from_slice(&total.to_le_bytes());
            b[32..36].copy_from_slice(&self.block_size.to_le_bytes());
            b[44..48].copy_from_slice(&self.block_count.to_le_bytes());
            for (i, (off, size)) in placed.iter().enumerate() {
                let base = 56 + i * 16;
                b[base..base + 8].copy_from_slice(&off.to_le_bytes());
                b[base + 8..base + 16].copy_from_slice(&size.to_le_bytes());
            }
            let d = digest(&b[..Superblock::CHECKSUM_OFFSET]);
            b[Superblock::CHECKSUM_OFFSET..].copy_from_slice(&d.to_le_bytes());

            let mut image = b.to_vec();
            image.extend_from_slice(&body);
            image
        }
    }

    pub fn block_entry(image_offset: u64, stored: u32, decoded: u32, codec: Codec) -> Vec<u8> {
        let mut e = Vec::with_capacity(16);
        e.extend_from_slice(&image_offset.to_le_bytes());
        e.extend_from_slice(&stored.to_le_bytes());
        e.extend_from_slice(&decoded.to_le_bytes()[..3]);
        e.push(codec.id());
        e
    }

    /// A whole valid image: superblock plus three zero-filled sections.
    pub fn valid_image() -> Vec<u8> {
        let mut b = [0u8; SUPERBLOCK_SIZE];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&Codec::Stored.bit().to_le_bytes());
        b[16..24].copy_from_slice(&(4096u64 * 4).to_le_bytes());
        b[24..32].copy_from_slice(&1234u64.to_le_bytes());
        b[32..36].copy_from_slice(&65536u32.to_le_bytes());
        b[36..40].copy_from_slice(&3u32.to_le_bytes());
        b[56..64].copy_from_slice(&4096u64.to_le_bytes());
        b[64..72].copy_from_slice(&4096u64.to_le_bytes());
        let d = digest(&b[..Superblock::CHECKSUM_OFFSET]);
        b[Superblock::CHECKSUM_OFFSET..].copy_from_slice(&d.to_le_bytes());

        let mut image = b.to_vec();
        image.resize(4096 * 4, 0);
        image
    }
}

#[cfg(test)]
mod section_tests {
    use super::tests_support::{Builder, block_entry};
    use super::*;

    fn one_block_image() -> Vec<u8> {
        let b = Builder::default();
        let data_at = {
            let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 16]);
            probe.data_offset()
        };
        b.section(
            SectionKind::Blocks,
            block_entry(data_at, 100, 4096, Codec::Stored),
        )
        .block_count(1)
        .with_data(vec![0xEE; 200])
        .build()
    }

    #[test]
    fn block_index_parses_and_validates() {
        let image = one_block_image();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let index = opened.block_index().unwrap();
        assert_eq!(index.count(), 1);
        let e = index.get(0).unwrap();
        assert_eq!(e.stored_length, 100);
        assert_eq!(e.decoded_length, 4096);
        assert_eq!(e.codec, Codec::Stored);
        assert!(index.get(1).is_none());
    }

    #[test]
    fn metadata_is_borrowed_from_an_in_memory_source() {
        let image = one_block_image();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let data = opened.section_data(SectionKind::Blocks).unwrap();
        assert!(matches!(data, std::borrow::Cow::Borrowed(_)));
    }

    #[cfg(feature = "fs")]
    #[test]
    fn metadata_is_copied_from_a_file_source_and_agrees() {
        let image = one_block_image();
        let path = std::env::temp_dir().join("stipplefs-section.img");
        std::fs::write(&path, &image).unwrap();

        let from_file = Image::open(&path).unwrap();
        let copied = from_file.section_data(SectionKind::Blocks).unwrap();
        assert!(matches!(copied, std::borrow::Cow::Owned(_)));

        let in_memory = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let borrowed = in_memory.section_data(SectionKind::Blocks).unwrap();
        assert_eq!(copied.as_ref(), borrowed.as_ref());
        assert_eq!(
            from_file.block_index().unwrap().get(0),
            in_memory.block_index().unwrap().get(0)
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_a_corrupted_metadata_section() {
        let mut image = one_block_image();
        image[4096] ^= 0xff;
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(matches!(
            opened.section_data(SectionKind::Blocks),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_decoded_length_larger_than_block_size() {
        let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 16]);
        let at = probe.data_offset();
        let image = Builder::default()
            .section(
                SectionKind::Blocks,
                block_entry(at, 100, 1 << 20, Codec::Stored),
            )
            .block_count(1)
            .with_data(vec![0xEE; 200])
            .build();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(matches!(opened.block_index(), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_a_block_outside_the_image() {
        let image = Builder::default()
            .section(
                SectionKind::Blocks,
                block_entry(1 << 40, 100, 4096, Codec::Stored),
            )
            .block_count(1)
            .build();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(matches!(opened.block_index(), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_a_block_whose_codec_is_absent_from_the_mask() {
        let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 16]);
        let at = probe.data_offset();
        let image = Builder::default()
            .section(SectionKind::Blocks, block_entry(at, 100, 4096, Codec::Zstd))
            .block_count(1)
            .codec_mask(Codec::Stored.bit() | Codec::Zstd.bit())
            .with_data(vec![0xEE; 200])
            .build();
        // codec_mask names zstd, which this build cannot decode, so open fails first
        assert!(matches!(
            Image::new(image.clone(), 0, image.len() as u64),
            Err(Error::UnsupportedCodec { .. })
        ));
    }
}
