//! On-disk constants and structures, as defined by `docs/format.md`.

/// Bytes an image begins with.
pub const MAGIC: [u8; 8] = *b"STIPPLE\0";

/// Size of the superblock, and the alignment every section starts on.
pub const SUPERBLOCK_SIZE: usize = 4096;

/// Alignment every section and the total image length is a multiple of.
pub const ALIGNMENT: u64 = 4096;

/// Major format version this build implements.
pub const VERSION_MAJOR: u16 = 1;

/// Minor format version this build writes.
pub const VERSION_MINOR: u16 = 0;

/// Number of entries in the section table.
pub const SECTION_COUNT: usize = 6;

/// Smallest permitted decoded block size.
pub const MIN_BLOCK_SIZE: u32 = 4096;

/// Largest permitted decoded block size, bounded by the three-byte
/// `decoded_length` field of a block index entry.
pub const MAX_BLOCK_SIZE: u32 = 1 << 23;

/// Index of each section within the section table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum SectionKind {
    /// Inode table.
    Inodes = 0,
    /// Directory entry table.
    Dirents = 1,
    /// Name table.
    Names = 2,
    /// Extent table.
    Extents = 3,
    /// Block index.
    Blocks = 4,
    /// Compression dictionaries.
    Dictionaries = 5,
}

/// How a block's bytes are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Stored bytes are the decoded bytes.
    Stored,
    /// Zstandard frame with no dictionary.
    Zstd,
    /// Zstandard frame using the image's dictionary.
    ZstdDict,
}

impl Codec {
    /// Recover a codec from its on-disk identifier.
    pub const fn from_id(id: u8) -> Option<Codec> {
        match id {
            0 => Some(Codec::Stored),
            1 => Some(Codec::Zstd),
            2 => Some(Codec::ZstdDict),
            _ => None,
        }
    }

    /// On-disk identifier.
    pub const fn id(self) -> u8 {
        match self {
            Codec::Stored => 0,
            Codec::Zstd => 1,
            Codec::ZstdDict => 2,
        }
    }

    /// Bit this codec occupies in the superblock's codec mask.
    pub const fn bit(self) -> u16 {
        1 << self.id()
    }
}

/// Codecs this build can decode.
pub const fn supported_codecs() -> u16 {
    Codec::Stored.bit()
}

/// Where a section lives within the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Section {
    /// Offset from the start of the image.
    pub offset: u64,
    /// Length in bytes, including the section's trailing digest.
    pub size: u64,
}

impl Section {
    /// Whether the section is absent.
    pub const fn is_absent(self) -> bool {
        self.size == 0
    }
}

/// The fields the superblock carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    /// Major format version.
    pub version_major: u16,
    /// Minor format version.
    pub version_minor: u16,
    /// Reserved in version 1.0.
    pub flags: u16,
    /// Bitmask of every codec used anywhere in the image.
    pub codec_mask: u16,
    /// Total size of the image in bytes.
    pub image_length: u64,
    /// Total length of the logical stream.
    pub stream_length: u64,
    /// Decoded size of a full block.
    pub block_size: u32,
    /// Number of inodes.
    pub inode_count: u32,
    /// Number of entries in the extent table.
    pub extent_count: u32,
    /// Number of entries in the block index.
    pub block_count: u32,
    /// Inode number of the root directory.
    pub root_inode: u32,
    /// Where each section lives.
    pub sections: [Section; SECTION_COUNT],
}

impl Superblock {
    /// Byte offset of the superblock's own digest.
    pub const CHECKSUM_OFFSET: usize = 4088;

    /// Look up one section.
    pub fn section(&self, kind: SectionKind) -> Section {
        self.sections[kind as usize]
    }
}
