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
pub const VERSION_MINOR: u16 = 1;

/// Lowest minor version this build can read.
///
/// The inode table's layout changed at minor version 1 and no reader can
/// interpret both, so anything older is refused rather than misread.
pub const MIN_VERSION_MINOR: u16 = 1;

/// Number of entries in the section table.
pub const SECTION_COUNT: usize = 8;

/// Smallest permitted decoded block size.
pub const MIN_BLOCK_SIZE: u32 = 4096;

/// Largest permitted decoded block size.
///
/// Large blocks compress better and cost more to read: serving one byte
/// decodes the whole block. The format does not pick a point on that curve,
/// it only bounds the field, so a writer may trade all the way to either end.
pub const MAX_BLOCK_SIZE: u32 = 1 << 31;

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
    /// Distinct mode, owner and group tuples.
    Attributes = 6,
    /// Extended attributes.
    ExtendedAttributes = 7,
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
    /// Zstandard frame using the preceding block's decoded bytes as its
    /// dictionary.
    ///
    /// Blocks pair up, so the block a span block refers to is always a plain
    /// one. Serving a span block costs one extra decode and never a chain.
    ZstdSpan,
}

impl Codec {
    /// Recover a codec from its on-disk identifier.
    pub const fn from_id(id: u8) -> Option<Codec> {
        match id {
            0 => Some(Codec::Stored),
            1 => Some(Codec::Zstd),
            2 => Some(Codec::ZstdDict),
            3 => Some(Codec::ZstdSpan),
            _ => None,
        }
    }

    /// On-disk identifier.
    pub const fn id(self) -> u8 {
        match self {
            Codec::Stored => 0,
            Codec::Zstd => 1,
            Codec::ZstdDict => 2,
            Codec::ZstdSpan => 3,
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
        mask |= Codec::ZstdDict.bit();
        mask |= Codec::ZstdSpan.bit();
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

/// Size of one block index entry.
pub const BLOCK_ENTRY_SIZE: usize = 20;

/// Size of the header that opens the inode table.
pub const INODE_HEADER_SIZE: usize = 16;

/// Size of one attribute table entry.
pub const ATTR_ENTRY_SIZE: usize = 12;

/// Size of one extended attribute pair.
pub const XATTR_PAIR_SIZE: usize = 12;

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
/// POSIX named pipe type.
pub const S_IFIFO: u32 = 0o010000;
/// POSIX socket type.
pub const S_IFSOCK: u32 = 0o140000;
/// POSIX character device type.
pub const S_IFCHR: u32 = 0o020000;
/// POSIX block device type.
pub const S_IFBLK: u32 = 0o060000;

/// What kind of thing an inode describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// A regular file.
    Regular,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// A named pipe.
    Fifo,
    /// A socket.
    Socket,
    /// A character device.
    CharDevice,
    /// A block device.
    BlockDevice,
}

impl FileType {
    /// The kind the type bits of `mode` name, if the format defines one.
    pub const fn from_mode(mode: u32) -> Option<FileType> {
        match mode & S_IFMT {
            S_IFREG => Some(FileType::Regular),
            S_IFDIR => Some(FileType::Directory),
            S_IFLNK => Some(FileType::Symlink),
            S_IFIFO => Some(FileType::Fifo),
            S_IFSOCK => Some(FileType::Socket),
            S_IFCHR => Some(FileType::CharDevice),
            S_IFBLK => Some(FileType::BlockDevice),
            _ => None,
        }
    }

    /// Whether this kind carries a device number.
    pub const fn is_device(self) -> bool {
        matches!(self, FileType::CharDevice | FileType::BlockDevice)
    }
}

/// What the inode arrays hold for one inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
    /// File size, directory entry count, or symbolic link target length.
    pub size: u64,
    /// Meaning depends on the kind; a device number for the two device kinds.
    pub payload: u64,
    /// Number of extents, zero for anything that is not a regular file.
    pub extent_count: u32,
    /// POSIX type and permission bits, from the attribute table.
    pub mode: u32,
    /// Owning user, from the attribute table.
    pub uid: u32,
    /// Owning group, from the attribute table.
    pub gid: u32,
    /// Number of names referring to this inode.
    pub nlink: u32,
    /// Seconds since the epoch, or `None` when the image records no times.
    pub mtime: Option<u64>,
    /// Index into the extended attribute table, or `None`.
    pub xattr: Option<u32>,
}

impl Inode {
    /// What kind of thing this inode describes, if the format defines it.
    pub fn file_type(&self) -> Option<FileType> {
        FileType::from_mode(self.mode)
    }

    /// The device number, for the kinds that carry one.
    pub fn device(&self) -> Option<u64> {
        self.file_type()
            .filter(|t| t.is_device())
            .map(|_| self.payload)
    }
}

/// The widths an image chose for its inode arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InodeWidths {
    /// Bytes per `size` entry.
    pub size: u8,
    /// Bytes per `payload` entry.
    pub payload: u8,
    /// Bytes per attribute index.
    pub attrs: u8,
    /// Bytes per link count.
    pub nlink: u8,
    /// Bytes per modification time, zero when none are recorded.
    pub mtime: u8,
    /// Bytes per extended attribute index, zero when none are recorded.
    pub xattr: u8,
    /// Bytes per extent count.
    pub extents: u8,
    /// Seconds the recorded times are measured from.
    pub mtime_base: u64,
}

impl InodeWidths {
    /// Bytes one inode costs across every array.
    pub const fn per_inode(&self) -> usize {
        self.size as usize
            + self.payload as usize
            + self.attrs as usize
            + self.nlink as usize
            + self.mtime as usize
            + self.xattr as usize
            + self.extents as usize
    }

    /// Whether every width lies in the range the format permits.
    pub const fn is_valid(&self) -> bool {
        matches!(self.size, 1..=8)
            && matches!(self.payload, 1..=8)
            && matches!(self.attrs, 1..=4)
            && matches!(self.nlink, 1..=4)
            && self.mtime <= 8
            && self.xattr <= 4
            && matches!(self.extents, 1..=4)
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
