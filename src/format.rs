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
///
/// A build selects its codecs, so a size-constrained reader can link only
/// what it needs and refuse images it cannot serve.
pub const fn supported_codecs() -> u16 {
    #[allow(unused_mut)]
    let mut mask = Codec::Stored.bit();
    #[cfg(feature = "zstd")]
    {
        mask |= Codec::Zstd.bit();
    }
    mask
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

/// Size of one inode table entry.
pub const INODE_SIZE: usize = 40;

/// Size of one extent table entry.
pub const EXTENT_SIZE: usize = 24;

/// Size of one directory entry.
pub const DIRENT_SIZE: usize = 12;

/// POSIX file type bits.
pub const S_IFMT: u32 = 0o170000;
/// POSIX regular file type.
pub const S_IFREG: u32 = 0o100000;
/// POSIX directory type.
pub const S_IFDIR: u32 = 0o040000;
/// POSIX symbolic link type.
pub const S_IFLNK: u32 = 0o120000;

/// What kind of thing an inode describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
}

/// One entry of the inode table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
    /// File size, directory entry count, or symbolic link target length.
    pub size: u64,
    /// Meaning depends on the file type.
    pub payload: u64,
    /// Number of extents, zero for anything that is not a regular file.
    pub extent_count: u32,
    /// POSIX type and permission bits.
    pub mode: u32,
    /// Owning user.
    pub uid: u32,
    /// Owning group.
    pub gid: u32,
    /// Number of names referring to this inode.
    pub nlink: u32,
}

impl Inode {
    /// What kind of thing this inode describes, if the format defines it.
    pub fn file_type(&self) -> Option<FileType> {
        match self.mode & S_IFMT {
            S_IFREG => Some(FileType::Regular),
            S_IFDIR => Some(FileType::Directory),
            S_IFLNK => Some(FileType::Symlink),
            _ => None,
        }
    }
}

/// One entry of the extent table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Where the content lives in the logical stream.
    pub stream_offset: u64,
    /// Where the extent begins within the file.
    pub file_offset: u64,
    /// Length in bytes.
    pub length: u32,
}

/// One directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dirent {
    /// Inode this name refers to.
    pub inode: u32,
    /// Byte offset of the name within the name table.
    pub name_offset: u32,
    /// Length of the name in bytes.
    pub name_length: u16,
}
