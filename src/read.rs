//! Opening an image and validating its structure.

use crate::error::{Error, Result};
use crate::format::{
    ALIGNMENT, Codec, MAGIC, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, SECTION_COUNT, SUPERBLOCK_SIZE,
    Section, Superblock, VERSION_MAJOR, supported_codecs,
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
