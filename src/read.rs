//! Opening an image and validating its structure.

use crate::error::{Error, Result};
use crate::format::{
    ALIGNMENT, ATTR_ENTRY_SIZE, BLOCK_ENTRY_SIZE, BLOCK_ENTRY_SIZE_VARIABLE, Codec, DIRENT_SIZE,
    Dirent, EXTENT_SIZE, Extent, FLAG_COMPRESSED_METADATA, FLAG_VARIABLE_BLOCKS, FileType,
    INODE_HEADER_SIZE, Inode, InodeWidths, KNOWN_CODEC_BITS, KNOWN_FLAG_BITS, MAGIC,
    MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, MIN_VERSION_MINOR, SECTION_COUNT, SUPERBLOCK_SIZE, Section,
    SectionKind, Superblock, VERSION_MAJOR, XATTR_PAIR_SIZE, supported_codecs,
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
pub(crate) fn digest(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

/// Parse and validate a superblock.
///
/// `available` is the number of bytes the caller said the image occupies. The
/// superblock's own `image_length` may not exceed it.
pub(crate) fn parse_superblock(
    bytes: &[u8; SUPERBLOCK_SIZE],
    available: u64,
) -> Result<Superblock> {
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
    let version_minor = u16at(bytes, 10);
    if version_minor < MIN_VERSION_MINOR {
        return Err(Error::UnsupportedMinorVersion {
            minor: version_minor,
            supported: MIN_VERSION_MINOR,
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
    if codec_mask & !KNOWN_CODEC_BITS != 0 {
        return Err(Error::Malformed {
            detail: "codec_mask names a codec the format does not define",
        });
    }
    let missing = codec_mask & !supported_codecs();
    if missing != 0 {
        return Err(Error::UnsupportedCodec { missing });
    }

    let flags = u16at(bytes, 12);
    if flags & !KNOWN_FLAG_BITS != 0 {
        return Err(Error::Malformed {
            detail: "the superblock sets a flag the format does not define",
        });
    }
    if flags & FLAG_COMPRESSED_METADATA != 0 && version_minor < 2 {
        return Err(Error::Malformed {
            detail: "compressed metadata is not defined before minor version 2",
        });
    }
    if flags & FLAG_VARIABLE_BLOCKS != 0 && version_minor < 4 {
        return Err(Error::Malformed {
            detail: "variable blocks are not defined before minor version 4",
        });
    }

    let stream_length = u64at(bytes, 24);
    // Reserved, and so ignored, before minor version 3.
    let hot_length = if version_minor >= 3 {
        u64at(bytes, 184)
    } else {
        0
    };
    if hot_length > stream_length {
        return Err(Error::Malformed {
            detail: "the hot region is longer than the stream",
        });
    }

    Ok(Superblock {
        version_major,
        version_minor,
        flags,
        codec_mask,
        image_length,
        stream_length,
        block_size,
        inode_count: u32at(bytes, 36),
        extent_count: u32at(bytes, 40),
        block_count: u32at(bytes, 44),
        root_inode: u32at(bytes, 48),
        sections,
        hot_length,
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

/// Read the widths that open the inode table, and check the arrays fit.
fn parse_inode_widths(section: &[u8], inode_count: u32) -> Result<InodeWidths> {
    if inode_count == 0 {
        return Ok(InodeWidths::default());
    }
    let head = section.get(..INODE_HEADER_SIZE).ok_or(Error::Malformed {
        detail: "the inode table is too short to hold its header",
    })?;
    let widths = InodeWidths {
        size: head[0],
        payload: head[1],
        attrs: head[2],
        nlink: head[3],
        mtime: head[4],
        xattr: head[5],
        extents: head[6],
        mtime_base: le_u64(&head[8..16]),
    };
    if !widths.is_valid() {
        return Err(Error::Malformed {
            detail: "an inode array width lies outside the permitted range",
        });
    }
    let needed = widths
        .per_inode()
        .checked_mul(inode_count as usize)
        .and_then(|n| n.checked_add(INODE_HEADER_SIZE))
        .ok_or(Error::Malformed {
            detail: "the inode arrays overflow",
        })?;
    if section.len() < needed {
        return Err(Error::Malformed {
            detail: "the inode table is shorter than its widths require",
        });
    }
    Ok(widths)
}

/// Expand a metadata section stored as a decoded-length prefix and a zstd
/// frame. The frame decompresses to exactly the recorded length, or the image
/// is malformed.
#[cfg(feature = "zstd")]
fn expand_section(stored: &[u8]) -> Result<Vec<u8>> {
    if stored.len() < 8 {
        return Err(Error::Malformed {
            detail: "a compressed section is too short to hold its decoded length",
        });
    }
    let decoded_length = u64::from_le_bytes(stored[..8].try_into().unwrap());
    if decoded_length > MAX_SECTION_BYTES {
        return Err(Error::Malformed {
            detail: "a compressed section declares an absurd decoded length",
        });
    }
    let mut d = zstd::bulk::Decompressor::new()?;
    let out = d
        .decompress(&stored[8..], decoded_length as usize)
        .map_err(|_| Error::Malformed {
            detail: "a compressed section failed to decompress",
        })?;
    if out.len() as u64 != decoded_length {
        return Err(Error::Malformed {
            detail: "a compressed section decoded to a length other than the one recorded",
        });
    }
    Ok(out)
}

/// Sections are built in memory by the writer, so a decoded length beyond
/// this can only be a corrupt length prefix.
#[cfg(feature = "zstd")]
const MAX_SECTION_BYTES: u64 = 1 << 40;

/// A build without zstd cannot expand a compressed section, so any image
/// that stores one is refused rather than misread.
#[cfg(not(feature = "zstd"))]
fn expand_section(_stored: &[u8]) -> Result<Vec<u8>> {
    Err(Error::Malformed {
        detail: "the image stores compressed metadata but this build has no zstd support",
    })
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
    /// `xxh3-64` of the stored bytes, recorded in the index only by images
    /// with [`FLAG_VARIABLE_BLOCKS`].
    pub digest: Option<u64>,
}

/// The block index, parsed on demand rather than materialised.
pub struct BlockIndex<'a> {
    bytes: SectionData<'a>,
    count: u32,
    block_size: u64,
    /// Stream offset of every block, present only for variable blocks.
    /// Fixed blocks start at multiples of `block_size`.
    starts: Option<Vec<u64>>,
}

impl BlockIndex<'_> {
    /// Number of blocks.
    pub fn count(&self) -> u32 {
        self.count
    }

    fn entry_size(&self) -> usize {
        if self.starts.is_some() {
            BLOCK_ENTRY_SIZE_VARIABLE
        } else {
            BLOCK_ENTRY_SIZE
        }
    }

    /// Entry `i`, or `None` when `i` is out of range.
    pub fn get(&self, i: u32) -> Option<BlockEntry> {
        if i >= self.count {
            return None;
        }
        let size = self.entry_size();
        let at = i as usize * size;
        let e = self.bytes.get(at..at + size)?;
        Some(BlockEntry {
            image_offset: le_u64(&e[0..8]),
            stored_length: le_u32(&e[8..12]),
            decoded_length: le_u32(&e[12..16]),
            codec: Codec::from_id(e[16])?,
            digest: (size == BLOCK_ENTRY_SIZE_VARIABLE).then(|| le_u64(&e[20..28])),
        })
    }

    /// Stream offset where block `i` begins, or `None` when `i` is out of
    /// range.
    pub fn start(&self, i: u32) -> Option<u64> {
        if i >= self.count {
            return None;
        }
        match &self.starts {
            Some(starts) => starts.get(i as usize).copied(),
            None => Some(i as u64 * self.block_size),
        }
    }

    /// The block holding stream byte `offset` and the offset within that
    /// block, or `None` when no block holds it.
    pub fn locate(&self, offset: u64) -> Option<(u32, u64)> {
        let index = match &self.starts {
            Some(starts) => {
                u32::try_from(starts.partition_point(|&s| s <= offset).checked_sub(1)?).ok()?
            }
            None => u32::try_from(offset / self.block_size).ok()?,
        };
        let within = offset - self.start(index)?;
        (within < self.get(index)?.decoded_length as u64).then_some((index, within))
    }
}

impl<'a> Image<'a> {
    /// Read a metadata section, verifying its digest.
    ///
    /// The bytes are borrowed when the underlying source can lend them and the
    /// image stores the section raw. A section stored compressed is expanded
    /// once here, so every caller sees the same bytes either way.
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
        let content_len = (section.size - 8) as usize;

        let compressed =
            self.superblock.flags & FLAG_COMPRESSED_METADATA != 0 && kind.is_compressible();

        match self.region.slice(section.offset, section.size) {
            Some(all) => {
                let (c, d) = all.split_at(content_len);
                let mut digest_bytes = [0u8; 8];
                digest_bytes.copy_from_slice(d);
                if digest(c) != u64::from_le_bytes(digest_bytes) {
                    return Err(Error::ChecksumMismatch {
                        what: "metadata section",
                    });
                }
                if !compressed {
                    return Ok(SectionData::Borrowed(c));
                }
                Ok(SectionData::Owned(expand_section(c)?))
            }
            None => {
                let mut c = vec![0u8; content_len];
                self.region.read_at(section.offset, &mut c)?;
                let mut d = [0u8; 8];
                self.region
                    .read_at(section.offset + content_len as u64, &mut d)?;
                if digest(&c) != u64::from_le_bytes(d) {
                    return Err(Error::ChecksumMismatch {
                        what: "metadata section",
                    });
                }
                if !compressed {
                    return Ok(SectionData::Owned(c));
                }
                Ok(SectionData::Owned(expand_section(&c)?))
            }
        }
    }

    /// Read and validate the block index.
    pub fn block_index(&self) -> Result<BlockIndex<'_>> {
        let bytes = self.section_data(SectionKind::Blocks)?;
        let count = self.superblock.block_count;
        let variable = self.superblock.flags & FLAG_VARIABLE_BLOCKS != 0;
        let entry_size = if variable {
            BLOCK_ENTRY_SIZE_VARIABLE
        } else {
            BLOCK_ENTRY_SIZE
        };
        let needed = (count as usize)
            .checked_mul(entry_size)
            .ok_or(Error::Malformed {
                detail: "block_count overflows the block index",
            })?;
        if bytes.len() < needed {
            return Err(Error::Malformed {
                detail: "block index section is shorter than block_count requires",
            });
        }
        let mut index = BlockIndex {
            bytes,
            count,
            block_size: self.superblock.block_size as u64,
            starts: None,
        };
        if variable {
            let mut starts = Vec::with_capacity(count as usize);
            let mut at = 0u64;
            for i in 0..count {
                let at_entry = i as usize * entry_size;
                let length = le_u32(&index.bytes[at_entry + 12..]);
                if length == 0 {
                    return Err(Error::Malformed {
                        detail: "a variable block decodes to nothing",
                    });
                }
                starts.push(at);
                at = at.checked_add(length as u64).ok_or(Error::Malformed {
                    detail: "the blocks' decoded lengths overflow",
                })?;
            }
            if at != self.superblock.stream_length {
                return Err(Error::Malformed {
                    detail: "the blocks' decoded lengths do not add up to stream_length",
                });
            }
            index.starts = Some(starts);
        }
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

/// Default bytes of decoded blocks a reader may retain.
///
/// A budget below the working set is a performance cliff rather than a gentle
/// slowdown: a walk in path order evicts and re-decodes continuously. The
/// default holds a few hundred blocks at the default block size, which does
/// not thrash a typical tree. Raise it for large working sets, lower it only
/// when memory is scarcer than time.
///
/// The budget bounds the cache, not the process. Resident memory is the
/// metadata, plus the cache, plus a constant for per-thread buffers. Measured
/// on a mount of `/usr/include`, reading six hundred files:
///
/// ```text
/// budget     4 MiB : idle 3,796 kB, peak 15,676 kB
/// budget    32 MiB : idle 3,876 kB, peak 25,264 kB
/// budget   256 MiB : idle 3,904 kB, peak 25,212 kB
/// ```
///
/// Those figures were measured with raw metadata sections; compressed ones
/// lower the idle floor. Idle memory does not move with the budget, because it
/// is the metadata floor. Peak memory rises with the budget and then stops
/// once the working set fits, which is why the largest budget buys nothing
/// over the default.
pub const DEFAULT_CACHE_BUDGET: usize = 32 << 20;

/// How many decoded blocks of headroom the default budget carries.
///
/// A reader working through large blocks needs room for several of them at
/// once: with a budget of two blocks, any interleaving of three files evicts
/// continuously. Eight keeps a directory walk and a handful of concurrent
/// readers from thrashing, and costs nothing until the working set actually
/// fills the budget.
pub const DEFAULT_BLOCKS_RETAINED: usize = 8;

/// The decoded bytes of a block.
///
/// Blocks that the source can lend and that need no decoding are borrowed
/// directly and cost no cache memory at all. Everything else is materialised
/// and retained within the budget.
pub enum Block<'a> {
    /// Borrowed from the byte source, costing no cache memory.
    Lent(&'a [u8]),
    /// Materialised and held by the cache.
    Cached(std::sync::Arc<Vec<u8>>),
}

impl std::ops::Deref for Block<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Block::Lent(b) => b,
            Block::Cached(b) => b,
        }
    }
}

/// How far a file's reads have advanced contiguously.
#[derive(Clone, Copy, Default)]
struct Run {
    ino: u32,
    /// Offset a read would have to start at to continue the run.
    next: u64,
    /// Contiguous reads seen so far.
    length: u32,
}

/// Files tracked for sequential access at once.
///
/// A fixed table keyed by inode, so the state cannot grow with the tree. A
/// collision loses one file's history, which costs a missed recognition and
/// nothing else.
const RUN_SLOTS: usize = 64;

/// Contiguous reads before a run counts as sequential.
const RUN_THRESHOLD: u32 = 4;

/// A borrowing view over an image's metadata, built once and reused.
///
/// Every section is verified when the reader is created, so later reads do
/// not re-check digests.
pub struct Reader<'i> {
    superblock: Superblock,
    region: &'i Region<'i>,
    inodes: SectionData<'i>,
    extents: SectionData<'i>,
    dirents: SectionData<'i>,
    names: SectionData<'i>,
    attrs: SectionData<'i>,
    xattrs: SectionData<'i>,
    widths: InodeWidths,
    blocks: BlockIndex<'i>,
    cache: crate::cache::BlockCache,
    runs: std::sync::Mutex<[Run; RUN_SLOTS]>,
    metrics: crate::metrics::Metrics,
    #[cfg(feature = "zstd")]
    dictionary: Option<zstd::dict::DecoderDictionary<'static>>,
}

// Counts extent table probes so tests can check that resolution does not scan
// a file's extents. Thread local because the harness runs tests in parallel and
// a shared counter would be inflated by whatever else is running.
#[cfg(test)]
thread_local! {
    pub(crate) static EXTENT_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le_u64(b: &[u8]) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[..8]);
    u64::from_le_bytes(v)
}

impl<'i> Reader<'i> {
    /// The image's superblock.
    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    /// The block index.
    pub fn blocks(&self) -> &BlockIndex<'i> {
        &self.blocks
    }

    /// Inode `ino`, or `None` when it does not exist.
    pub fn inode(&self, ino: u32) -> Option<Inode> {
        if ino >= self.superblock.inode_count {
            return None;
        }
        let w = &self.widths;
        let i = ino as usize;
        let n = self.superblock.inode_count as usize;
        let body = self.inodes.get(INODE_HEADER_SIZE..)?;

        // Each array is contiguous, so a field is one indexed read.
        let mut at = 0usize;
        let mut take = |width: u8| -> Option<u64> {
            if width == 0 {
                return None;
            }
            let base = at + i * width as usize;
            let bytes = body.get(base..base + width as usize)?;
            at += n * width as usize;
            let mut v = [0u8; 8];
            v[..bytes.len()].copy_from_slice(bytes);
            Some(u64::from_le_bytes(v))
        };

        let size = take(w.size)?;
        let payload = take(w.payload)?;
        let attrs = take(w.attrs)? as u32;
        let nlink = take(w.nlink)? as u32;
        let mtime = take(w.mtime).map(|t| w.mtime_base + t);
        let xattr = take(w.xattr)
            .and_then(|v| v.checked_sub(1))
            .map(|v| v as u32);
        let extent_count = take(w.extents)? as u32;

        let (mode, uid, gid) = self.attribute(attrs)?;
        Some(Inode {
            size,
            payload,
            extent_count,
            mode,
            uid,
            gid,
            nlink,
            mtime,
            xattr,
        })
    }

    /// The mode, owner and group an attribute table entry holds.
    fn attribute(&self, index: u32) -> Option<(u32, u32, u32)> {
        let at = index as usize * ATTR_ENTRY_SIZE;
        let e = self.attrs.get(at..at + ATTR_ENTRY_SIZE)?;
        Some((le_u32(&e[0..4]), le_u32(&e[4..8]), le_u32(&e[8..12])))
    }

    /// Extent table entry `i`.
    pub fn extent(&self, i: u32) -> Option<Extent> {
        #[cfg(test)]
        EXTENT_PROBES.with(|c| c.set(c.get() + 1));
        if i >= self.superblock.extent_count {
            return None;
        }
        let at = i as usize * EXTENT_SIZE;
        let e = self.extents.get(at..at + EXTENT_SIZE)?;
        Some(Extent {
            stream_offset: le_u64(&e[0..8]),
            file_offset: le_u64(&e[8..16]),
            length: le_u32(&e[16..20]),
        })
    }

    /// Directory entry `i`.
    pub fn dirent(&self, i: u32) -> Option<Dirent> {
        let at = i as usize * DIRENT_SIZE;
        let e = self.dirents.get(at..at + DIRENT_SIZE)?;
        Some(Dirent {
            inode: le_u32(&e[0..4]),
            name_offset: le_u32(&e[4..8]),
            name_length: le_u16(&e[8..10]),
        })
    }

    /// The bytes of a directory entry's name.
    pub fn name(&self, entry: Dirent) -> Option<&[u8]> {
        let at = entry.name_offset as usize;
        self.names.get(at..at + entry.name_length as usize)
    }

    /// The extended attributes recorded for an inode, as name and value pairs.
    ///
    /// An inode with none yields an empty vector rather than an error.
    pub fn xattrs(&self, ino: u32) -> Result<Vec<(&[u8], &[u8])>> {
        let Some(set) = self.inode(ino).and_then(|n| n.xattr) else {
            return Ok(Vec::new());
        };
        let s = &self.xattrs;
        let malformed = |detail| Error::Malformed { detail };

        let head = s
            .get(..8)
            .ok_or(malformed("the extended attribute section is too short"))?;
        let set_count = le_u32(&head[0..4]) as usize;
        let pair_count = le_u32(&head[4..8]) as usize;
        if set as usize >= set_count {
            return Err(malformed(
                "an inode names an attribute set that does not exist",
            ));
        }

        let starts_at = 8;
        let pairs_at = starts_at + 4 * (set_count + 1);
        let pool_at = pairs_at + XATTR_PAIR_SIZE * pair_count;
        let starts = s
            .get(starts_at..pairs_at)
            .ok_or(malformed("the attribute set index is truncated"))?;
        let pool = s
            .get(pool_at..)
            .ok_or(malformed("the attribute pool is truncated"))?;

        let first = le_u32(&starts[set as usize * 4..]) as usize;
        let last = le_u32(&starts[(set as usize + 1) * 4..]) as usize;
        if first > last || last > pair_count {
            return Err(malformed("an attribute set spans an impossible range"));
        }

        let mut out = Vec::with_capacity(last - first);
        for i in first..last {
            let at = pairs_at + i * XATTR_PAIR_SIZE;
            let e = s
                .get(at..at + XATTR_PAIR_SIZE)
                .ok_or(malformed("an attribute pair is truncated"))?;
            let name_off = le_u32(&e[0..4]) as usize;
            let name_len = le_u16(&e[4..6]) as usize;
            let value_off = le_u32(&e[6..10]) as usize;
            let value_len = le_u16(&e[10..12]) as usize;
            let name = pool
                .get(name_off..name_off + name_len)
                .ok_or(malformed("an attribute name lies outside the pool"))?;
            let value = pool
                .get(value_off..value_off + value_len)
                .ok_or(malformed("an attribute value lies outside the pool"))?;
            out.push((name, value));
        }
        Ok(out)
    }

    /// The bytes a symbolic link points at.
    pub fn symlink_target(&self, ino: u32) -> Option<&[u8]> {
        let node = self.inode(ino)?;
        if node.file_type() != Some(FileType::Symlink) {
            return None;
        }
        let at = usize::try_from(node.payload).ok()?;
        let len = usize::try_from(node.size).ok()?;
        self.names.get(at..at + len)
    }

    /// Find a child of `dir` by name.
    ///
    /// Directory entries are sorted by name, so this is a binary search.
    pub fn lookup(&self, dir: u32, name: &[u8]) -> Option<u32> {
        let node = self.inode(dir)?;
        if node.file_type() != Some(FileType::Directory) {
            return None;
        }
        let first = u32::try_from(node.payload).ok()?;
        let mut lo = 0u32;
        let mut hi = u32::try_from(node.size).ok()?;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = self.dirent(first + mid)?;
            match self.name(entry)?.cmp(name) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(entry.inode),
            }
        }
        None
    }

    /// Resolve a slash separated path from the root.
    ///
    /// An empty path, or one made only of separators, resolves to the root.
    pub fn resolve(&self, path: &[u8]) -> Option<u32> {
        let mut ino = self.root();
        for part in path.split(|&b| b == b'/') {
            if part.is_empty() || part == b"." {
                continue;
            }
            ino = self.lookup(ino, part)?;
        }
        Some(ino)
    }

    /// The root directory's inode number.
    pub fn root(&self) -> u32 {
        self.superblock.root_inode
    }

    /// The entries of a directory, in name order.
    pub fn entries(&self, dir: u32) -> impl Iterator<Item = (&[u8], u32)> + '_ {
        let node = self
            .inode(dir)
            .filter(|n| n.file_type() == Some(FileType::Directory));
        let first = node.map_or(0, |n| n.payload as u32);
        let count = node.map_or(0, |n| n.size as u32);
        (0..count).filter_map(move |i| {
            let entry = self.dirent(first + i)?;
            Some((self.name(entry)?, entry.inode))
        })
    }

    /// Read a whole file.
    ///
    /// Prefer [`Reader::read`] when only part of a file is wanted; this exists
    /// for the common case of small files.
    pub fn read_all(&self, ino: u32) -> Result<Vec<u8>> {
        let node = self.inode(ino).ok_or(Error::Malformed {
            detail: "read of an inode that does not exist",
        })?;
        let mut out = vec![
            0u8;
            usize::try_from(node.size).map_err(|_| Error::Malformed {
                detail: "file is larger than this platform can address",
            })?
        ];
        let mut at = 0usize;
        while at < out.len() {
            let n = self.read(ino, at as u64, &mut out[at..])?;
            if n == 0 {
                break;
            }
            at += n;
        }
        out.truncate(at);
        Ok(out)
    }

    /// Check that a regular file's extents tile it exactly.
    fn check_tiling(&self, inode: &Inode) -> Result<()> {
        if inode.extent_count == 0 {
            return if inode.size == 0 {
                Ok(())
            } else {
                Err(Error::Malformed {
                    detail: "a file with no extents has a non-zero size",
                })
            };
        }
        if inode.extent_count == 1 {
            return Ok(());
        }
        let first = u32::try_from(inode.payload).map_err(|_| Error::Malformed {
            detail: "a file's first extent index does not fit the extent table",
        })?;
        let last = first
            .checked_add(inode.extent_count)
            .ok_or(Error::Malformed {
                detail: "a file's extent run overflows the extent table",
            })?;
        if last > self.superblock.extent_count {
            return Err(Error::Malformed {
                detail: "a file's extent run runs past the end of the extent table",
            });
        }
        let mut expected = 0u64;
        for i in first..last {
            let e = self.extent(i).ok_or(Error::Malformed {
                detail: "a file references an extent that cannot be read",
            })?;
            if e.file_offset != expected {
                return Err(Error::Malformed {
                    detail: "a file's extents leave a gap or overlap",
                });
            }
            expected = expected
                .checked_add(e.length as u64)
                .ok_or(Error::Malformed {
                    detail: "a file's extent lengths overflow",
                })?;
        }
        if expected != inode.size {
            return Err(Error::Malformed {
                detail: "a file's extent lengths do not sum to its recorded size",
            });
        }
        Ok(())
    }

    /// Find the extent covering `offset`, returning it and how many of its
    /// bytes remain from that point.
    fn locate(&self, inode: &Inode, offset: u64) -> Result<(u64, u64)> {
        if inode.extent_count == 1 {
            let remaining = inode.size - offset;
            return Ok((inode.payload + offset, remaining));
        }
        let first = inode.payload as u32;
        let count = inode.extent_count;
        let mut lo = 0u32;
        let mut hi = count;
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.extent(first + mid).ok_or(Error::Malformed {
                detail: "a file references an extent that cannot be read",
            })?;
            if e.file_offset <= offset {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let e = self.extent(first + lo).ok_or(Error::Malformed {
            detail: "a file references an extent that cannot be read",
        })?;
        let within = offset - e.file_offset;
        Ok((e.stream_offset + within, e.length as u64 - within))
    }

    /// Counters describing where read time went.
    ///
    /// These stay at zero unless the crate is built with the `metrics`
    /// feature; the instrumentation compiles away otherwise.
    pub fn metrics(&self) -> &crate::metrics::Metrics {
        &self.metrics
    }

    #[inline]
    fn note(&self, _pick: fn(&crate::metrics::Metrics) -> &std::sync::atomic::AtomicU64) {
        #[cfg(feature = "metrics")]
        self.metrics.bump(_pick(&self.metrics));
    }

    #[inline]
    fn note_n(
        &self,
        _pick: fn(&crate::metrics::Metrics) -> &std::sync::atomic::AtomicU64,
        _n: u64,
    ) {
        #[cfg(feature = "metrics")]
        self.metrics.add(_pick(&self.metrics), _n);
    }

    #[inline]
    fn phase<T>(
        &self,
        _pick: fn(&crate::metrics::Metrics) -> &std::sync::atomic::AtomicU64,
        f: impl FnOnce() -> T,
    ) -> T {
        #[cfg(feature = "metrics")]
        {
            self.metrics.timed(_pick(&self.metrics), f)
        }
        #[cfg(not(feature = "metrics"))]
        f()
    }

    /// Bytes this reader's cache currently holds.
    pub fn cache_used(&self) -> usize {
        self.cache.used()
    }

    /// Bytes this reader's cache is permitted to hold.
    pub fn cache_budget(&self) -> usize {
        self.cache.budget()
    }

    /// The decoded bytes of one block.
    /// Record where this read leaves the file, and say whether the file is
    /// being streamed rather than read at scattered offsets.
    ///
    /// Recognition only decides whether a decoded block is worth keeping. It
    /// never changes which bytes a read returns.
    fn note_access(&self, ino: u32, offset: u64, len: u64) -> bool {
        let slot = ino as usize % RUN_SLOTS;
        let Ok(mut runs) = self.runs.lock() else {
            return false;
        };
        let run = &mut runs[slot];
        if run.ino == ino && run.next == offset {
            run.length = run.length.saturating_add(1);
            run.next = offset + len;
            if run.length == RUN_THRESHOLD {
                self.note(|m| &m.sequential_runs);
            }
            if run.length > RUN_THRESHOLD {
                self.note(|m| &m.sequential_continued);
            }
            return run.length >= RUN_THRESHOLD;
        }
        if run.ino == ino && run.length >= RUN_THRESHOLD {
            self.note(|m| &m.sequential_broken);
        }
        *run = Run {
            ino,
            next: offset + len,
            length: 1,
        };
        false
    }

    fn block(&self, index: u32, streaming: bool) -> Result<Block<'_>> {
        self.note(|m| &m.block_fetches);
        if streaming {
            self.note(|m| &m.streamed_blocks);
        }
        let e = self.blocks.get(index).ok_or(Error::Malformed {
            detail: "a stream offset falls outside the block index",
        })?;
        let stored_len = e.stored_length as u64;

        if e.codec == Codec::Stored {
            if e.stored_length != e.decoded_length {
                return Err(Error::Malformed {
                    detail: "a stored block's stored and decoded lengths differ",
                });
            }
            if let Some(all) = self.region.slice(e.image_offset, stored_len + 8) {
                let (b, tail) = all.split_at(e.stored_length as usize);
                self.check_block(&e, b, tail)?;
                self.note(|m| &m.lent);
                return Ok(Block::Lent(b));
            }
        }

        let before = self.cache.decodes();
        let bytes = self.phase(
            |m| &m.fetch_ns,
            || {
                self.cache
                    .get_or_decode(index, streaming, || self.decode(e))
            },
        )?;
        if self.cache.decodes() == before {
            self.note(|m| &m.cache_hits);
        } else {
            self.note(|m| &m.cache_misses);
        }
        Ok(Block::Cached(bytes))
    }

    /// The stream ranges holding a regular file's bytes, as
    /// `(stream_offset, file_offset, length)` in ascending file order.
    ///
    /// Empty for anything that is not a regular file with content.
    pub fn file_extents(&self, ino: u32) -> Vec<(u64, u64, u64)> {
        let Some(node) = self.inode(ino) else {
            return Vec::new();
        };
        if node.file_type() != Some(FileType::Regular) {
            return Vec::new();
        }
        match node.extent_count {
            0 => Vec::new(),
            1 => vec![(node.payload, 0, node.size)],
            n => (0..n)
                .filter_map(|k| self.extent(node.payload as u32 + k))
                .map(|e| (e.stream_offset, e.file_offset, e.length as u64))
                .collect(),
        }
    }

    /// The decoded bytes of block `index`, decoding it only when the cache
    /// does not already hold it.
    pub fn block_bytes(&self, index: u32) -> Result<Block<'_>> {
        self.block(index, false)
    }

    /// Drop block `index` from the cache, for a caller that knows no read
    /// will want it again soon.
    pub fn forget_block(&self, index: u32) {
        self.cache.remove(index);
    }

    /// The block holding byte `offset` of regular file `ino`, or `None` when
    /// the file has no byte there.
    pub fn block_of(&self, ino: u32, offset: u64) -> Option<u32> {
        let inode = self.inode(ino)?;
        if inode.file_type() != Some(FileType::Regular) || offset >= inode.size {
            return None;
        }
        let (stream_offset, _) = self.locate(&inode, offset).ok()?;
        self.blocks.locate(stream_offset).map(|(index, _)| index)
    }

    /// Check a block's stored bytes against the digest that follows them,
    /// and that digest against the block index when the index records one.
    fn check_block(&self, e: &BlockEntry, stored: &[u8], trailing: &[u8]) -> Result<()> {
        let want = le_u64(trailing);
        if e.digest.is_some_and(|d| d != want) {
            return Err(Error::Malformed {
                detail: "a block's digest disagrees with its block index entry",
            });
        }
        if self.phase(|m| &m.digest_ns, || digest(stored)) != want {
            return Err(Error::ChecksumMismatch { what: "block" });
        }
        Ok(())
    }

    /// Fetch, verify and decode one block's stored bytes.
    fn decode(&self, e: BlockEntry) -> Result<Vec<u8>> {
        let region = self.region;
        // The digest sits immediately after the block, so one call fetches
        // both. Reading it separately doubled the calls into the source.
        let mut raw = vec![0u8; e.stored_length as usize + 8];
        self.note(|m| &m.source_calls);
        self.note_n(|m| &m.source_bytes, raw.len() as u64);
        self.phase(
            |m| &m.source_ns,
            || region.read_at(e.image_offset, &mut raw),
        )?;
        let (stored, tail) = raw.split_at(e.stored_length as usize);
        self.check_block(&e, stored, tail)?;
        raw.truncate(e.stored_length as usize);
        let decoded = match e.codec {
            Codec::Stored => raw,
            #[cfg(feature = "zstd")]
            Codec::Zstd => {
                // The decoded size is recorded, so decompress into one
                // exactly sized allocation. `decode_all` does not know the
                // size and grows its buffer as it goes.
                let mut d = zstd::bulk::Decompressor::new()?;
                self.phase(
                    |m| &m.decode_ns,
                    || d.decompress(&raw, e.decoded_length as usize),
                )?
            }
            #[cfg(not(feature = "zstd"))]
            Codec::Zstd => {
                return Err(Error::UnsupportedCodec {
                    missing: e.codec.bit(),
                });
            }
            #[cfg(feature = "zstd")]
            Codec::ZstdBcj => {
                let mut d = zstd::bulk::Decompressor::new()?;
                let mut out = self.phase(
                    |m| &m.decode_ns,
                    || d.decompress(&raw, e.decoded_length as usize),
                )?;
                self.phase(|m| &m.decode_ns, || crate::bcj::decode(&mut out));
                out
            }
            #[cfg(not(feature = "zstd"))]
            Codec::ZstdBcj => {
                return Err(Error::UnsupportedCodec {
                    missing: e.codec.bit(),
                });
            }
            #[cfg(feature = "zstd")]
            Codec::ZstdDict => {
                let dict = self.dictionary.as_ref().ok_or(Error::Malformed {
                    detail: "a block uses the image dictionary but none is present",
                })?;
                let mut d = self.phase(
                    |m| &m.dctx_ns,
                    || zstd::bulk::Decompressor::with_prepared_dictionary(dict),
                )?;
                self.phase(
                    |m| &m.decode_ns,
                    || d.decompress(&raw, e.decoded_length as usize),
                )?
            }
            #[cfg(not(feature = "zstd"))]
            Codec::ZstdDict => {
                return Err(Error::UnsupportedCodec {
                    missing: e.codec.bit(),
                });
            }
            #[cfg(feature = "zstd")]
            Codec::ZstdDictBcj => {
                let dict = self.dictionary.as_ref().ok_or(Error::Malformed {
                    detail: "a block uses the image dictionary but none is present",
                })?;
                let mut d = self.phase(
                    |m| &m.dctx_ns,
                    || zstd::bulk::Decompressor::with_prepared_dictionary(dict),
                )?;
                let mut out = self.phase(
                    |m| &m.decode_ns,
                    || d.decompress(&raw, e.decoded_length as usize),
                )?;
                self.phase(|m| &m.decode_ns, || crate::bcj::decode(&mut out));
                out
            }
            #[cfg(not(feature = "zstd"))]
            Codec::ZstdDictBcj => {
                return Err(Error::UnsupportedCodec {
                    missing: e.codec.bit(),
                });
            }
        };
        if decoded.len() as u32 != e.decoded_length {
            return Err(Error::Malformed {
                detail: "a block decoded to a length other than the one recorded",
            });
        }
        Ok(decoded)
    }

    /// Read from a regular file into `buf`, returning how many bytes were read.
    ///
    /// A read starting inside the file and extending past its end returns the
    /// bytes that exist. A read starting at or after the end returns zero.
    pub fn read(&self, ino: u32, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let inode = self.inode(ino).ok_or(Error::Malformed {
            detail: "read of an inode that does not exist",
        })?;
        if inode.file_type() != Some(FileType::Regular) {
            return Err(Error::Malformed {
                detail: "read of something that is not a regular file",
            });
        }
        self.check_tiling(&inode)?;
        if offset >= inode.size {
            return Ok(0);
        }
        let want = (inode.size - offset).min(buf.len() as u64) as usize;

        self.note(|m| &m.reads);
        let streaming = self.note_access(ino, offset, want as u64);
        let mut done = 0usize;
        while done < want {
            let at = offset + done as u64;
            let (stream_offset, extent_left) =
                self.phase(|m| &m.resolve_ns, || self.locate(&inode, at))?;
            if stream_offset >= self.superblock.stream_length {
                return Err(Error::Malformed {
                    detail: "an extent points past the end of the stream",
                });
            }
            let (block_index, within) =
                self.blocks.locate(stream_offset).ok_or(Error::Malformed {
                    detail: "a stream offset falls outside the block index",
                })?;
            let within = within as usize;
            let bytes = self.block(block_index, streaming)?;
            let available = bytes.len().checked_sub(within).ok_or(Error::Malformed {
                detail: "an extent points past the end of its block",
            })?;
            if available == 0 {
                return Err(Error::Malformed {
                    detail: "an extent points at the end of its block",
                });
            }
            let n = available.min(extent_left as usize).min(want - done);
            self.phase(
                |m| &m.copy_ns,
                || buf[done..done + n].copy_from_slice(&bytes[within..within + n]),
            );
            done += n;
        }
        self.note_n(|m| &m.read_bytes, done as u64);
        Ok(done)
    }
}

impl<'a> Image<'a> {
    /// Build a reader with a cache budget suited to the image.
    ///
    /// The default holds a few hundred blocks at a small block size. An image
    /// built with very large blocks needs room for several of them at once, so
    /// the budget grows with the block size, never below
    /// [`DEFAULT_CACHE_BUDGET`].
    pub fn reader(&self) -> Result<Reader<'_>> {
        self.reader_with_budget(self.default_budget())
    }

    /// The budget [`Image::reader`] would use.
    pub fn default_budget(&self) -> usize {
        let retained =
            (self.superblock.block_size as usize).saturating_mul(DEFAULT_BLOCKS_RETAINED);
        DEFAULT_CACHE_BUDGET.max(retained)
    }

    /// Build a reader whose decoded-block cache holds at most `budget` bytes.
    ///
    /// Verifies every metadata section once, so later reads do not re-check
    /// digests.
    pub fn reader_with_budget(&self, budget: usize) -> Result<Reader<'_>> {
        crate::cache::check_budget(budget, self.superblock.block_size as usize)?;
        let inodes = self.section_data(SectionKind::Inodes)?;
        let widths = parse_inode_widths(&inodes, self.superblock.inode_count)?;
        Ok(Reader {
            superblock: self.superblock,
            region: &self.region,
            attrs: self.section_data(SectionKind::Attributes)?,
            xattrs: self.section_data(SectionKind::ExtendedAttributes)?,
            widths,
            inodes,
            extents: self.section_data(SectionKind::Extents)?,
            dirents: self.section_data(SectionKind::Dirents)?,
            names: self.section_data(SectionKind::Names)?,
            blocks: self.block_index()?,
            cache: crate::cache::BlockCache::new(budget),
            runs: std::sync::Mutex::new([Run::default(); RUN_SLOTS]),
            metrics: crate::metrics::Metrics::default(),
            #[cfg(feature = "zstd")]
            dictionary: {
                let d = self.section_data(SectionKind::Dictionaries)?;
                if d.is_empty() {
                    if self.superblock.codec_mask
                        & (Codec::ZstdDict.bit() | Codec::ZstdDictBcj.bit())
                        != 0
                    {
                        return Err(Error::Malformed {
                            detail: "codec_mask names a dictionary codec but no dictionary is present",
                        });
                    }
                    None
                } else {
                    Some(zstd::dict::DecoderDictionary::copy(&d))
                }
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SectionKind;
    use crate::format::VERSION_MINOR;

    /// Builds a superblock byte by byte at the offsets `docs/format.md` gives,
    /// so the fixture exercises the specification rather than the writer.
    fn fixture() -> [u8; SUPERBLOCK_SIZE] {
        let mut b = [0u8; SUPERBLOCK_SIZE];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        b[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
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
        assert_eq!(sb.version_minor, VERSION_MINOR);
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
        b[10..12].copy_from_slice(&(VERSION_MINOR + 7).to_le_bytes());
        seal(&mut b);
        assert_eq!(
            parse_superblock(&b, 4096 * 4).unwrap().version_minor,
            VERSION_MINOR + 7
        );
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
        // No power of two above MAX_BLOCK_SIZE fits in the field, so the
        // rejected cases are zero, too small, and not a power of two.
        for bad in [0u32, 2048, 65535, u32::MAX] {
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

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn rejects_a_codec_this_build_cannot_decode() {
        let mut b = fixture();
        b[14..16].copy_from_slice(&(Codec::Stored.bit() | Codec::ZstdDict.bit()).to_le_bytes());
        seal(&mut b);
        let err = parse_superblock(&b, 4096 * 4).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedCodec { missing } if missing == Codec::ZstdDict.bit()),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_a_codec_the_format_does_not_define() {
        let mut b = fixture();
        b[14..16].copy_from_slice(&0x8000u16.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_a_flag_the_format_does_not_define() {
        let mut b = fixture();
        b[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
        b[12..14].copy_from_slice(&0x8000u16.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_compressed_metadata_before_minor_version_two() {
        let mut b = fixture();
        b[10..12].copy_from_slice(&1u16.to_le_bytes());
        b[12..14].copy_from_slice(&FLAG_COMPRESSED_METADATA.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn reads_the_hot_region_and_rejects_one_past_the_stream() {
        let mut b = fixture();
        b[184..192].copy_from_slice(&1000u64.to_le_bytes());
        seal(&mut b);
        assert_eq!(parse_superblock(&b, 4096 * 4).unwrap().hot_length, 1000);

        b[184..192].copy_from_slice(&1235u64.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            parse_superblock(&b, 4096 * 4),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn ignores_the_hot_region_bytes_before_minor_version_three() {
        let mut b = fixture();
        b[10..12].copy_from_slice(&2u16.to_le_bytes());
        b[184..192].copy_from_slice(&u64::MAX.to_le_bytes());
        seal(&mut b);
        assert_eq!(parse_superblock(&b, 4096 * 4).unwrap().hot_length, 0);
    }

    #[test]
    fn accepts_the_compressed_metadata_flag() {
        let mut b = fixture();
        b[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
        b[12..14].copy_from_slice(&FLAG_COMPRESSED_METADATA.to_le_bytes());
        seal(&mut b);
        assert_eq!(
            parse_superblock(&b, 4096 * 4).unwrap().flags,
            FLAG_COMPRESSED_METADATA
        );
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

    #[cfg(feature = "zstd")]
    #[test]
    fn expands_a_compressed_section() {
        let decoded: Vec<u8> = (0..10_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let image = super::tests_support::Builder::default()
            .compressed_section(SectionKind::Names, &decoded)
            .build();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert_eq!(
            opened.superblock().flags & FLAG_COMPRESSED_METADATA,
            FLAG_COMPRESSED_METADATA
        );
        assert_eq!(
            &opened.section_data(SectionKind::Names).unwrap()[..],
            &decoded[..]
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn rejects_a_compressed_section_that_decodes_to_the_wrong_length() {
        let decoded = vec![7u8; 5_000];
        let frame = zstd::encode_all(&decoded[..], 3).unwrap();
        let mut stored = Vec::new();
        stored.extend_from_slice(&1_000u64.to_le_bytes());
        stored.extend_from_slice(&frame);
        let image = super::tests_support::Builder::default()
            .section(SectionKind::Names, stored)
            .flags(FLAG_COMPRESSED_METADATA)
            .build();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(opened.section_data(SectionKind::Names).is_err());
    }

    #[test]
    fn a_raw_section_survives_the_flag_only_when_it_is_excluded() {
        // Blocks and Dictionaries stay raw even when the flag is set, so a
        // fixture storing them raw must read back unchanged.
        let decoded = vec![3u8; 4_000];
        let image = super::tests_support::Builder::default()
            .section(SectionKind::Blocks, decoded.clone())
            .flags(FLAG_COMPRESSED_METADATA)
            .build();
        let opened = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert_eq!(
            &opened.section_data(SectionKind::Blocks).unwrap()[..],
            &decoded[..]
        );
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
    use crate::format::VERSION_MINOR;

    /// Lays out a whole image: superblock, 4096-aligned sections each with a
    /// trailing digest, then a data area.
    pub struct Builder {
        pub sections: Vec<(SectionKind, Vec<u8>)>,
        pub block_count: u32,
        pub codec_mask: u16,
        pub flags: u16,
        pub block_size: u32,
        pub data: Vec<u8>,
        pub stream_length: u64,
        pub inode_count: u32,
        pub extent_count: u32,
    }

    impl Default for Builder {
        fn default() -> Self {
            Builder {
                sections: Vec::new(),
                block_count: 0,
                codec_mask: Codec::Stored.bit(),
                flags: 0,
                block_size: 65536,
                data: Vec::new(),
                stream_length: 0,
                inode_count: 0,
                extent_count: 0,
            }
        }
    }

    impl Builder {
        pub fn section(mut self, kind: SectionKind, content: Vec<u8>) -> Self {
            self.sections.push((kind, content));
            self
        }

        /// Store `decoded` as a compressed section, the way the writer does
        /// when the compressed-metadata flag is set.
        #[cfg(feature = "zstd")]
        pub fn compressed_section(mut self, kind: SectionKind, decoded: &[u8]) -> Self {
            let frame = zstd::encode_all(decoded, 3).unwrap();
            let mut stored = Vec::with_capacity(8 + frame.len());
            stored.extend_from_slice(&(decoded.len() as u64).to_le_bytes());
            stored.extend_from_slice(&frame);
            self.flags |= FLAG_COMPRESSED_METADATA;
            self.sections.push((kind, stored));
            self
        }

        pub fn flags(mut self, f: u16) -> Self {
            self.flags = f;
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
            b[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
            b[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
            b[12..14].copy_from_slice(&self.flags.to_le_bytes());
            b[14..16].copy_from_slice(&self.codec_mask.to_le_bytes());
            b[16..24].copy_from_slice(&total.to_le_bytes());
            b[24..32].copy_from_slice(&self.stream_length.to_le_bytes());
            b[32..36].copy_from_slice(&self.block_size.to_le_bytes());
            b[36..40].copy_from_slice(&self.inode_count.to_le_bytes());
            b[40..44].copy_from_slice(&self.extent_count.to_le_bytes());
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

    /// Lays out a whole small filesystem: a stream split into stored blocks,
    /// plus the metadata sections describing it.
    pub struct Fs {
        pub stream: Vec<u8>,
        pub dictionary: Option<Vec<u8>>,
        pub codec: Codec,
        pub block_size: u32,
        pub inodes: Vec<Inode>,
        pub extents: Vec<Extent>,
        pub dirents: Vec<Dirent>,
        pub names: Vec<u8>,
    }

    /// Encode inodes the way `docs/format.md` describes: a header of widths,
    /// then one array per field. Written independently of the writer so the
    /// fixtures exercise the specification rather than the implementation.
    fn enc_inodes(records: &[Inode]) -> (Vec<u8>, Vec<u8>) {
        if records.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut tuples: Vec<(u32, u32, u32)> =
            records.iter().map(|r| (r.mode, r.uid, r.gid)).collect();
        tuples.sort_unstable();
        tuples.dedup();

        let mut attrs = Vec::new();
        for (mode, uid, gid) in &tuples {
            attrs.extend_from_slice(&mode.to_le_bytes());
            attrs.extend_from_slice(&uid.to_le_bytes());
            attrs.extend_from_slice(&gid.to_le_bytes());
        }

        let w = |vals: &[u64]| -> u8 {
            let max = vals.iter().copied().max().unwrap_or(0);
            ((64 - max.leading_zeros()).div_ceil(8)).max(1) as u8
        };
        let sizes: Vec<u64> = records.iter().map(|r| r.size).collect();
        let payloads: Vec<u64> = records.iter().map(|r| r.payload).collect();
        let nlinks: Vec<u64> = records.iter().map(|r| r.nlink as u64).collect();
        let extents: Vec<u64> = records.iter().map(|r| r.extent_count as u64).collect();
        let widths = (
            w(&sizes),
            w(&payloads),
            w(&[tuples.len().saturating_sub(1) as u64]),
            w(&nlinks),
            w(&extents),
        );

        let mut out = vec![widths.0, widths.1, widths.2, widths.3, 0, 0, widths.4, 0];
        out.extend_from_slice(&0u64.to_le_bytes());
        let push = |v: u64, width: u8, out: &mut Vec<u8>| {
            out.extend_from_slice(&v.to_le_bytes()[..width as usize]);
        };
        for v in &sizes {
            push(*v, widths.0, &mut out);
        }
        for v in &payloads {
            push(*v, widths.1, &mut out);
        }
        for r in records {
            let at = tuples
                .iter()
                .position(|t| *t == (r.mode, r.uid, r.gid))
                .expect("tuple present");
            push(at as u64, widths.2, &mut out);
        }
        for v in &nlinks {
            push(*v, widths.3, &mut out);
        }
        for v in &extents {
            push(*v, widths.4, &mut out);
        }
        (out, attrs)
    }

    fn enc_extent(e: &Extent) -> Vec<u8> {
        let mut v = Vec::with_capacity(EXTENT_SIZE);
        v.extend_from_slice(&e.stream_offset.to_le_bytes());
        v.extend_from_slice(&e.file_offset.to_le_bytes());
        v.extend_from_slice(&e.length.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    fn enc_dirent(d: &Dirent) -> Vec<u8> {
        let mut v = Vec::with_capacity(DIRENT_SIZE);
        v.extend_from_slice(&d.inode.to_le_bytes());
        v.extend_from_slice(&d.name_offset.to_le_bytes());
        v.extend_from_slice(&d.name_length.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v
    }

    impl Fs {
        pub fn build(&self) -> Vec<u8> {
            let bs = self.block_size as usize;
            let blocks: Vec<&[u8]> = self.stream.chunks(bs).collect();

            let (inodes, attributes) = enc_inodes(&self.inodes);
            let dirents: Vec<u8> = self.dirents.iter().flat_map(enc_dirent).collect();
            let extents: Vec<u8> = self.extents.iter().flat_map(enc_extent).collect();
            let block_section_len = blocks.len() * BLOCK_ENTRY_SIZE;

            // An empty section is absent, so it occupies no space at all.
            let present: Vec<(SectionKind, Vec<u8>)> = [
                (SectionKind::Inodes, inodes),
                (SectionKind::Dirents, dirents),
                (SectionKind::Names, self.names.clone()),
                (SectionKind::Extents, extents),
                (SectionKind::Attributes, attributes),
            ]
            .into_iter()
            .chain(
                self.dictionary
                    .clone()
                    .map(|d| (SectionKind::Dictionaries, d)),
            )
            .filter(|(_, c)| !c.is_empty())
            .collect();

            let padded = |n: usize| -> u64 { ((n as u64 + 8).div_ceil(4096)) * 4096 };
            let mut data_at = 4096u64;
            for (_, c) in &present {
                data_at += padded(c.len());
            }
            if block_section_len > 0 {
                data_at += padded(block_section_len);
            }

            let stored: Vec<Vec<u8>> = blocks
                .iter()
                .map(|b| match self.codec {
                    #[cfg(feature = "zstd")]
                    Codec::Zstd => zstd::encode_all(&b[..], 3).unwrap(),
                    #[cfg(feature = "zstd")]
                    Codec::ZstdDict => {
                        let dict = self.dictionary.as_deref().unwrap_or_default();
                        let mut c = zstd::bulk::Compressor::with_dictionary(3, dict).unwrap();
                        c.compress(b).unwrap()
                    }
                    _ => b.to_vec(),
                })
                .collect();

            let mut block_index = Vec::with_capacity(block_section_len);
            let mut at = data_at;
            for (raw, enc) in blocks.iter().zip(&stored) {
                block_index.extend(block_entry(
                    at,
                    enc.len() as u32,
                    raw.len() as u32,
                    self.codec,
                ));
                at += enc.len() as u64 + 8;
            }

            let mut data = Vec::new();
            for enc in &stored {
                data.extend_from_slice(enc);
                data.extend_from_slice(&digest(enc).to_le_bytes());
            }

            let mut builder = Builder::default()
                .block_count(blocks.len() as u32)
                .codec_mask(Codec::Stored.bit() | self.codec.bit())
                .with_data(data);
            builder.block_size = self.block_size;
            builder.stream_length = self.stream.len() as u64;
            builder.inode_count = self.inodes.len() as u32;
            builder.extent_count = self.extents.len() as u32;
            for (kind, content) in present {
                builder = builder.section(kind, content);
            }
            if block_section_len > 0 {
                builder = builder.section(SectionKind::Blocks, block_index);
            }
            builder.build()
        }
    }

    pub fn block_entry(image_offset: u64, stored: u32, decoded: u32, codec: Codec) -> Vec<u8> {
        let mut e = Vec::with_capacity(BLOCK_ENTRY_SIZE);
        e.extend_from_slice(&image_offset.to_le_bytes());
        e.extend_from_slice(&stored.to_le_bytes());
        e.extend_from_slice(&decoded.to_le_bytes());
        e.push(codec.id());
        e.extend_from_slice(&[0u8; 3]);
        e
    }

    /// A whole valid image: superblock plus three zero-filled sections.
    pub fn valid_image() -> Vec<u8> {
        let mut b = [0u8; SUPERBLOCK_SIZE];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        b[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
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

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn rejects_a_block_whose_codec_is_absent_from_the_mask() {
        let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 16]);
        let at = probe.data_offset();
        let image = Builder::default()
            .section(
                SectionKind::Blocks,
                block_entry(at, 100, 4096, Codec::ZstdDict),
            )
            .block_count(1)
            .codec_mask(Codec::Stored.bit() | Codec::ZstdDict.bit())
            .with_data(vec![0xEE; 200])
            .build();
        // codec_mask names zstd, which this build cannot decode, so open fails first
        assert!(matches!(
            Image::new(image.clone(), 0, image.len() as u64),
            Err(Error::UnsupportedCodec { .. })
        ));
    }
}

#[cfg(all(test, feature = "mmap"))]
mod mmap_tests {
    use super::tests_support::{Builder, block_entry};
    use super::*;
    use crate::source::MmapSource;

    #[test]
    fn a_mapped_image_lends_its_metadata() {
        let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 16]);
        let at = probe.data_offset();
        let image = Builder::default()
            .section(
                SectionKind::Blocks,
                block_entry(at, 100, 4096, Codec::Stored),
            )
            .block_count(1)
            .with_data(vec![0xEE; 200])
            .build();

        let path = std::env::temp_dir().join("stipplefs-mmap.img");
        std::fs::write(&path, &image).unwrap();
        let file = std::fs::File::open(&path).unwrap();

        // SAFETY: the file is written once above and not touched again while mapped.
        let source = unsafe { MmapSource::new(&file) }.unwrap();
        let mapped = Image::new(source, 0, image.len() as u64).unwrap();

        let data = mapped.section_data(SectionKind::Blocks).unwrap();
        assert!(
            matches!(data, std::borrow::Cow::Borrowed(_)),
            "a mapped image must lend metadata rather than copy it"
        );

        let in_memory = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert_eq!(
            mapped.block_index().unwrap().get(0),
            in_memory.block_index().unwrap().get(0)
        );
        std::fs::remove_file(&path).ok();
    }
}

#[cfg(test)]
mod robustness_tests {
    use super::tests_support::{Builder, block_entry};
    use super::*;

    /// Deterministic generator, so a failing case is reproducible from its seed.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    fn sound_image() -> Vec<u8> {
        let probe = Builder::default().section(SectionKind::Blocks, vec![0u8; 32]);
        let at = probe.data_offset();
        let mut blocks = block_entry(at, 100, 4096, Codec::Stored);
        blocks.extend(block_entry(at + 108, 50, 4096, Codec::Stored));
        Builder::default()
            .section(SectionKind::Blocks, blocks)
            .section(SectionKind::Inodes, vec![7u8; 120])
            .block_count(2)
            .with_data(vec![0xEE; 300])
            .build()
    }

    /// Exercises every public entry point. Any outcome is acceptable except a
    /// panic, an out-of-range read, or a hang.
    fn poke(bytes: &[u8]) {
        let len = bytes.len() as u64;
        if let Ok(image) = Image::new(bytes.to_vec(), 0, len) {
            let _ = image.section_data(SectionKind::Blocks);
            let _ = image.section_data(SectionKind::Inodes);
            let _ = image.section_data(SectionKind::Names);
            if let Ok(index) = image.block_index() {
                for i in 0..index.count().min(1000) {
                    let _ = index.get(i);
                }
                let _ = index.get(u32::MAX);
            }
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut rng = Rng(0x5754_4950_504C_4501);
        for _ in 0..4000 {
            let n = rng.below(9000);
            let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            poke(&bytes);
        }
    }

    #[test]
    fn corrupted_images_never_panic() {
        let sound = sound_image();
        let mut rng = Rng(0x0BAD_C0DE_1234_5678);
        for _ in 0..20_000 {
            let mut bytes = sound.clone();
            for _ in 0..1 + rng.below(6) {
                let at = rng.below(bytes.len());
                match rng.below(3) {
                    0 => bytes[at] ^= 1 << rng.below(8),
                    1 => bytes[at] = rng.next() as u8,
                    _ => bytes[at] = 0xff,
                }
            }
            poke(&bytes);
        }
    }

    #[test]
    fn truncated_images_never_panic() {
        let sound = sound_image();
        for n in 0..sound.len().min(9000) {
            poke(&sound[..n]);
        }
        let mut rng = Rng(0x7EAD_BEEF_0000_0001);
        for _ in 0..2000 {
            let n = rng.below(sound.len());
            poke(&sound[..n]);
        }
    }

    /// Recomputes every digest so a mutation reaches the validation logic
    /// instead of being stopped at an integrity check. Without this, checksums
    /// shield exactly the fields whose validation most needs exercising.
    fn reseal(bytes: &mut [u8]) {
        if bytes.len() < SUPERBLOCK_SIZE {
            return;
        }
        for i in 0..SECTION_COUNT {
            let base = 56 + i * 16;
            let mut o = [0u8; 8];
            let mut z = [0u8; 8];
            o.copy_from_slice(&bytes[base..base + 8]);
            z.copy_from_slice(&bytes[base + 8..base + 16]);
            let (off, size) = (u64::from_le_bytes(o), u64::from_le_bytes(z));
            if size < 8 {
                continue;
            }
            let (Ok(start), Ok(end)) = (usize::try_from(off), usize::try_from(off + size)) else {
                continue;
            };
            if end > bytes.len() || start < SUPERBLOCK_SIZE {
                continue;
            }
            let d = digest(&bytes[start..end - 8]).to_le_bytes();
            bytes[end - 8..end].copy_from_slice(&d);
        }
        let d = digest(&bytes[..Superblock::CHECKSUM_OFFSET]).to_le_bytes();
        bytes[Superblock::CHECKSUM_OFFSET..SUPERBLOCK_SIZE].copy_from_slice(&d);
    }

    #[test]
    fn resealed_corruption_never_panics() {
        let sound = sound_image();
        let mut rng = Rng(0xFEED_FACE_DEAD_BEEF);
        for _ in 0..20_000 {
            let mut bytes = sound.clone();
            for _ in 0..1 + rng.below(5) {
                let at = rng.below(bytes.len());
                match rng.below(4) {
                    0 => bytes[at] ^= 1 << rng.below(8),
                    1 => bytes[at] = rng.next() as u8,
                    2 => bytes[at] = 0xff,
                    _ => bytes[at] = 0,
                }
            }
            reseal(&mut bytes);
            poke(&bytes);
        }
    }

    #[test]
    fn resealed_truncation_never_panics() {
        let sound = sound_image();
        let mut rng = Rng(0xC0FF_EE00_1111_2222);
        for _ in 0..3000 {
            let n = rng.below(sound.len());
            let mut bytes = sound[..n].to_vec();
            reseal(&mut bytes);
            poke(&bytes);
        }
    }

    #[test]
    fn a_sound_image_still_reads_after_all_that() {
        let sound = sound_image();
        let image = Image::new(sound.clone(), 0, sound.len() as u64).unwrap();
        assert_eq!(image.block_index().unwrap().count(), 2);
    }
}

#[cfg(test)]
mod read_tests {
    use super::tests_support::Fs;
    use super::*;
    use crate::format::{S_IFDIR, S_IFLNK, S_IFREG};

    const NAMES: &[u8] = b"alphabetalinktarget/path";

    fn stream() -> Vec<u8> {
        (0..8192u32).map(|i| (i % 251) as u8).collect()
    }

    pub fn fs() -> Fs {
        Fs {
            stream: stream(),
            dictionary: None,
            codec: Codec::Stored,
            block_size: 4096,
            inodes: vec![
                Inode {
                    size: 3,
                    payload: 0,
                    extent_count: 0,
                    mode: S_IFDIR | 0o755,
                    uid: 0,
                    gid: 0,
                    nlink: 2,
                    mtime: None,
                    xattr: None,
                },
                Inode {
                    size: 100,
                    payload: 0,
                    extent_count: 1,
                    mode: S_IFREG | 0o644,
                    uid: 1000,
                    gid: 1000,
                    nlink: 1,
                    mtime: None,
                    xattr: None,
                },
                Inode {
                    size: 250,
                    payload: 0,
                    extent_count: 2,
                    mode: S_IFREG | 0o644,
                    uid: 1000,
                    gid: 1000,
                    nlink: 1,
                    mtime: None,
                    xattr: None,
                },
                Inode {
                    size: 11,
                    payload: 13,
                    extent_count: 0,
                    mode: S_IFLNK | 0o777,
                    uid: 0,
                    gid: 0,
                    nlink: 1,
                    mtime: None,
                    xattr: None,
                },
            ],
            extents: vec![
                Extent {
                    stream_offset: 4000,
                    file_offset: 0,
                    length: 200,
                },
                Extent {
                    stream_offset: 100,
                    file_offset: 200,
                    length: 50,
                },
            ],
            dirents: vec![
                Dirent {
                    inode: 1,
                    name_offset: 0,
                    name_length: 5,
                },
                Dirent {
                    inode: 2,
                    name_offset: 5,
                    name_length: 4,
                },
                Dirent {
                    inode: 3,
                    name_offset: 9,
                    name_length: 4,
                },
            ],
            names: NAMES.to_vec(),
        }
    }

    fn expected_beta() -> Vec<u8> {
        let s = stream();
        let mut v = s[4000..4200].to_vec();
        v.extend_from_slice(&s[100..150]);
        v
    }

    #[test]
    fn reads_a_single_extent_file() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 100];
        assert_eq!(r.read(1, 0, &mut buf).unwrap(), 100);
        assert_eq!(buf, stream()[0..100]);
    }

    #[test]
    fn reads_a_file_whose_extent_spans_a_block_boundary() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 250];
        assert_eq!(r.read(2, 0, &mut buf).unwrap(), 250);
        assert_eq!(buf, expected_beta());
    }

    #[test]
    fn partial_reads_agree_with_the_whole() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let whole = expected_beta();
        for off in 0..250u64 {
            for len in [1usize, 7, 64, 200] {
                let mut buf = vec![0u8; len];
                let n = r.read(2, off, &mut buf).unwrap();
                let want = (250 - off as usize).min(len);
                assert_eq!(n, want, "offset {off} len {len}");
                assert_eq!(buf[..n], whole[off as usize..off as usize + n]);
            }
        }
    }

    #[test]
    fn a_read_past_the_end_returns_only_what_exists() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0xCCu8; 500];
        assert_eq!(r.read(1, 90, &mut buf).unwrap(), 10);
        assert_eq!(buf[..10], stream()[90..100]);
        assert!(buf[10..].iter().all(|&b| b == 0xCC));
    }

    #[test]
    fn a_read_at_or_after_the_end_returns_nothing() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 16];
        assert_eq!(r.read(1, 100, &mut buf).unwrap(), 0);
        assert_eq!(r.read(1, 10_000, &mut buf).unwrap(), 0);
    }

    #[test]
    fn reading_a_directory_or_link_fails() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 16];
        assert!(r.read(0, 0, &mut buf).is_err());
        assert!(r.read(3, 0, &mut buf).is_err());
        assert!(r.read(99, 0, &mut buf).is_err());
    }

    #[test]
    fn rejects_extents_that_do_not_tile() {
        for bad in [
            vec![
                Extent {
                    stream_offset: 4000,
                    file_offset: 0,
                    length: 200,
                },
                Extent {
                    stream_offset: 100,
                    file_offset: 210,
                    length: 50,
                },
            ],
            vec![
                Extent {
                    stream_offset: 4000,
                    file_offset: 0,
                    length: 200,
                },
                Extent {
                    stream_offset: 100,
                    file_offset: 190,
                    length: 50,
                },
            ],
            vec![
                Extent {
                    stream_offset: 4000,
                    file_offset: 0,
                    length: 200,
                },
                Extent {
                    stream_offset: 100,
                    file_offset: 200,
                    length: 40,
                },
            ],
        ] {
            let mut f = fs();
            f.extents = bad;
            let image = f.build();
            let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
            let r = img.reader().unwrap();
            let mut buf = vec![0u8; 250];
            assert!(r.read(2, 0, &mut buf).is_err());
        }
    }

    #[test]
    fn directory_entries_and_names_are_readable() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let root = r.inode(0).unwrap();
        assert_eq!(root.file_type(), Some(FileType::Directory));
        let got: Vec<(u32, Vec<u8>)> = (0..root.size as u32)
            .map(|i| {
                let d = r.dirent(root.payload as u32 + i).unwrap();
                (d.inode, r.name(d).unwrap().to_vec())
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (1u32, b"alpha".to_vec()),
                (2, b"beta".to_vec()),
                (3, b"link".to_vec())
            ]
        );
        let mut sorted = got.clone();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(got, sorted, "directory entries must be sorted by name");
    }

    #[test]
    fn a_symbolic_link_target_is_readable() {
        let image = fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let link = r.inode(3).unwrap();
        assert_eq!(link.file_type(), Some(FileType::Symlink));
        let target = &NAMES[link.payload as usize..link.payload as usize + link.size as usize];
        assert_eq!(target, b"target/path");
    }

    #[test]
    fn resolution_does_not_scan_a_fragmented_file() {
        let n = 4096u32;
        let f = Fs {
            stream: stream(),
            dictionary: None,
            codec: Codec::Stored,
            block_size: 4096,
            inodes: vec![Inode {
                size: n as u64,
                payload: 0,
                extent_count: n,
                mode: S_IFREG | 0o644,
                uid: 0,
                gid: 0,
                nlink: 1,
                mtime: None,
                xattr: None,
            }],
            extents: (0..n)
                .map(|i| Extent {
                    stream_offset: i as u64,
                    file_offset: i as u64,
                    length: 1,
                })
                .collect(),
            dirents: vec![],
            names: vec![],
        };
        let image = f.build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();

        let mut buf = [0u8; 1];
        EXTENT_PROBES.with(|c| c.set(0));
        assert_eq!(r.read(0, (n - 1) as u64, &mut buf).unwrap(), 1);
        let probes = EXTENT_PROBES.with(|c| c.get());
        assert_eq!(buf[0], stream()[(n - 1) as usize]);

        // check_tiling walks the run once; resolution itself must be logarithmic.
        let budget = n as usize + 2 * (n as f64).log2().ceil() as usize + 8;
        assert!(
            probes <= budget,
            "resolution probed {probes} extents, budget {budget}"
        );
        let resolution_only = probes - n as usize;
        assert!(
            resolution_only <= 2 * (n as f64).log2().ceil() as usize + 4,
            "resolution alone probed {resolution_only} extents for {n}"
        );
    }
}

#[cfg(test)]
mod cache_integration_tests {
    use super::read_tests::fs as small_fs;
    use super::*;

    #[test]
    fn a_lendable_stored_image_costs_no_cache_memory() {
        let image = small_fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 250];
        r.read(2, 0, &mut buf).unwrap();
        assert_eq!(
            r.cache_used(),
            0,
            "a stored block from a lendable source must not be copied into the cache"
        );
    }

    #[cfg(feature = "fs")]
    #[test]
    fn a_file_backed_image_uses_the_cache_and_respects_the_budget() {
        let image = small_fs().build();
        let path = std::env::temp_dir().join("stipplefs-cache.img");
        std::fs::write(&path, &image).unwrap();

        let img = Image::open(&path).unwrap();
        let r = img.reader_with_budget(8192).unwrap();
        let mut buf = vec![0u8; 250];
        assert_eq!(r.read(2, 0, &mut buf).unwrap(), 250);

        let in_memory = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let rm = in_memory.reader().unwrap();
        let mut want = vec![0u8; 250];
        rm.read(2, 0, &mut want).unwrap();

        assert_eq!(buf, want, "cached and lent paths must agree");
        assert!(
            r.cache_used() > 0,
            "a file-backed read must populate the cache"
        );
        assert!(r.cache_used() <= r.cache_budget());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_budget_below_one_block_is_rejected() {
        let image = small_fs().build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(img.reader_with_budget(64).is_err());
        assert!(img.reader_with_budget(4096).is_ok());
    }
}

#[cfg(all(test, feature = "zstd"))]
mod zstd_tests {
    use super::read_tests::fs as base_fs;
    use super::*;

    #[test]
    fn a_zstd_image_reads_identically_to_a_stored_one() {
        let mut compressed = base_fs();
        compressed.codec = Codec::Zstd;
        let a = compressed.build();
        let b = base_fs().build();

        let total = |img: &[u8]| -> u32 {
            let i = Image::new(img.to_vec(), 0, img.len() as u64).unwrap();
            let idx = i.block_index().unwrap();
            (0..idx.count())
                .map(|k| idx.get(k).unwrap().stored_length)
                .sum()
        };
        assert!(
            total(&a) < total(&b),
            "compression should shrink the stored bytes: {} vs {}",
            total(&a),
            total(&b)
        );

        let ia = Image::new(a.clone(), 0, a.len() as u64).unwrap();
        let ib = Image::new(b.clone(), 0, b.len() as u64).unwrap();
        let (ra, rb) = (ia.reader().unwrap(), ib.reader().unwrap());

        for ino in [1u32, 2] {
            let size = ra.inode(ino).unwrap().size as usize;
            let mut want = vec![0u8; size];
            let mut got = vec![0u8; size];
            rb.read(ino, 0, &mut want).unwrap();
            ra.read(ino, 0, &mut got).unwrap();
            assert_eq!(got, want, "inode {ino} differs between codecs");
        }
    }

    #[test]
    fn a_compressed_block_is_cached_rather_than_lent() {
        let mut compressed = base_fs();
        compressed.codec = Codec::Zstd;
        let image = compressed.build();
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 100];
        r.read(1, 0, &mut buf).unwrap();
        assert!(
            r.cache_used() > 0,
            "a compressed block must be materialised into the cache"
        );
        assert!(r.cache_used() <= r.cache_budget());
    }

    #[test]
    fn a_block_that_decodes_to_the_wrong_length_is_rejected() {
        let mut compressed = base_fs();
        compressed.codec = Codec::Zstd;
        let mut image = compressed.build();

        // Overstate the first block's decoded length in the block index.
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let blocks = img.superblock().section(SectionKind::Blocks);
        let at = blocks.offset as usize + 12;
        image[at..at + 3].copy_from_slice(&[0xff, 0x0f, 0x00]);
        let end = (blocks.offset + blocks.size) as usize;
        let d = digest(&image[blocks.offset as usize..end - 8]).to_le_bytes();
        image[end - 8..end].copy_from_slice(&d);
        let sb_d = digest(&image[..Superblock::CHECKSUM_OFFSET]).to_le_bytes();
        image[Superblock::CHECKSUM_OFFSET..SUPERBLOCK_SIZE].copy_from_slice(&sb_d);

        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        let mut buf = vec![0u8; 100];
        assert!(r.read(1, 0, &mut buf).is_err());
    }

    /// Rewrite one inode's entry in a parallel array, then repair the section
    /// and superblock digests so the reader gets as far as the field itself
    /// rather than stopping at a checksum.
    fn with_inode_field(field: usize, ino: usize, value: u64) -> Vec<u8> {
        let mut image = base_fs().build();
        let (offset, size) = {
            let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
            let s = img.superblock().section(SectionKind::Inodes);
            (s.offset as usize, s.size as usize)
        };
        let n = {
            let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
            img.superblock().inode_count as usize
        };
        let widths: Vec<u8> = image[offset..offset + 7].to_vec();
        let width = widths[field] as usize;
        assert!(width > 0, "array {field} is not present in this fixture");
        let mut at = offset + INODE_HEADER_SIZE;
        for w in &widths[..field] {
            at += n * *w as usize;
        }
        at += ino * width;
        image[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);

        let end = offset + size;
        let d = digest(&image[offset..end - 8]).to_le_bytes();
        image[end - 8..end].copy_from_slice(&d);
        let sb = digest(&image[..Superblock::CHECKSUM_OFFSET]).to_le_bytes();
        image[Superblock::CHECKSUM_OFFSET..SUPERBLOCK_SIZE].copy_from_slice(&sb);
        image
    }

    #[test]
    fn an_inode_naming_an_attribute_beyond_the_table_is_refused() {
        // Field 2 is the attribute index. Pointing it past the table must not
        // yield somebody else's mode, owner and group.
        let image = with_inode_field(2, 1, u64::MAX);
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        assert!(
            r.inode(1).is_none(),
            "an attribute index past the table resolved to something"
        );
        let mut buf = vec![0u8; 10];
        assert!(r.read(1, 0, &mut buf).is_err());
    }

    #[test]
    fn an_inode_naming_extended_attributes_beyond_the_table_is_refused() {
        let widths = {
            let image = base_fs().build();
            let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
            let s = img.superblock().section(SectionKind::Inodes);
            image[s.offset as usize..s.offset as usize + 7].to_vec()
        };
        if widths[5] == 0 {
            // The fixture carries no extended attributes, so there is no index
            // to corrupt and nothing to check here.
            return;
        }
        let image = with_inode_field(5, 1, u64::MAX);
        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let r = img.reader().unwrap();
        assert!(
            r.xattrs(1).is_err() || r.xattrs(1).map(|v| v.is_empty()).unwrap_or(false),
            "an extended attribute index past the table returned attributes"
        );
    }
}

#[cfg(all(test, feature = "zstd"))]
mod dictionary_tests {
    use super::read_tests::fs as base_fs;
    use super::*;

    fn dict() -> Vec<u8> {
        (0..2048u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn a_dictionary_compressed_image_reads_correctly() {
        let mut f = base_fs();
        f.codec = Codec::ZstdDict;
        f.dictionary = Some(dict());
        let image = f.build();

        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert_eq!(
            img.superblock().codec_mask & Codec::ZstdDict.bit(),
            Codec::ZstdDict.bit()
        );
        let r = img.reader().unwrap();

        let plain = base_fs().build();
        let ip = Image::new(plain.clone(), 0, plain.len() as u64).unwrap();
        let rp = ip.reader().unwrap();

        for ino in [1u32, 2] {
            let size = r.inode(ino).unwrap().size as usize;
            let mut got = vec![0u8; size];
            let mut want = vec![0u8; size];
            r.read(ino, 0, &mut got).unwrap();
            rp.read(ino, 0, &mut want).unwrap();
            assert_eq!(
                got, want,
                "inode {ino} differs under dictionary compression"
            );
        }
    }

    #[test]
    fn a_dictionary_shrinks_the_stored_bytes() {
        let stored_total = |img: &[u8]| -> u32 {
            let i = Image::new(img.to_vec(), 0, img.len() as u64).unwrap();
            let idx = i.block_index().unwrap();
            (0..idx.count())
                .map(|k| idx.get(k).unwrap().stored_length)
                .sum()
        };
        let mut plain = base_fs();
        plain.codec = Codec::Zstd;
        let mut with_dict = base_fs();
        with_dict.codec = Codec::ZstdDict;
        with_dict.dictionary = Some(dict());

        let a = stored_total(&with_dict.build());
        let b = stored_total(&plain.build());
        assert!(a < b, "dictionary should shrink stored bytes: {a} vs {b}");
    }

    #[test]
    fn an_image_naming_the_dictionary_codec_without_one_is_rejected() {
        let mut f = base_fs();
        f.codec = Codec::ZstdDict;
        f.dictionary = Some(dict());
        let mut image = f.build();

        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let d = img.superblock().section(SectionKind::Dictionaries);
        assert!(!d.is_absent());

        // Blank the dictionaries section entry, keeping the codec mask.
        let base = 56 + (SectionKind::Dictionaries as usize) * 16;
        image[base..base + 16].copy_from_slice(&[0u8; 16]);
        let sb = digest(&image[..Superblock::CHECKSUM_OFFSET]).to_le_bytes();
        image[Superblock::CHECKSUM_OFFSET..SUPERBLOCK_SIZE].copy_from_slice(&sb);

        let img = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        assert!(
            img.reader().is_err(),
            "an image using the dictionary codec without a dictionary must be rejected"
        );
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::read_tests::fs as base_fs;
    use super::*;

    #[test]
    fn corruption_is_detected_in_every_present_section() {
        let image = base_fs().build();
        let probe = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let present: Vec<SectionKind> = [
            SectionKind::Inodes,
            SectionKind::Dirents,
            SectionKind::Names,
            SectionKind::Extents,
            SectionKind::Blocks,
        ]
        .into_iter()
        .filter(|k| !probe.superblock().section(*k).is_absent())
        .collect();
        assert!(
            present.len() >= 4,
            "fixture should exercise several sections"
        );

        for kind in present {
            let mut bad = image.clone();
            let at = probe.superblock().section(kind).offset as usize;
            bad[at] ^= 0xff;
            let img = Image::new(bad.clone(), 0, bad.len() as u64).unwrap();
            assert!(
                matches!(img.section_data(kind), Err(Error::ChecksumMismatch { .. })),
                "corruption in {kind:?} went undetected"
            );
        }
    }

    #[test]
    fn corrupted_block_content_yields_an_error_and_no_bytes() {
        let image = base_fs().build();
        let probe = Image::new(image.clone(), 0, image.len() as u64).unwrap();
        let first = probe.block_index().unwrap().get(0).unwrap();

        let mut bad = image.clone();
        bad[first.image_offset as usize] ^= 0xff;
        let img = Image::new(bad.clone(), 0, bad.len() as u64).unwrap();
        let r = img.reader().unwrap();

        let mut buf = vec![0xAAu8; 100];
        let outcome = r.read(1, 0, &mut buf);
        assert!(
            matches!(outcome, Err(Error::ChecksumMismatch { what: "block" })),
            "expected an integrity error, got {outcome:?}"
        );
        assert!(
            buf.iter().all(|&b| b == 0xAA),
            "corrupted bytes must not reach the caller"
        );
    }

    #[test]
    fn a_corrupted_superblock_is_detected_before_anything_else() {
        let mut bad = base_fs().build();
        bad[36] ^= 0xff;
        assert!(matches!(
            Image::new(bad.clone(), 0, bad.len() as u64),
            Err(Error::ChecksumMismatch { what: "superblock" })
        ));
    }
}

#[cfg(test)]
mod version_tests {
    use super::tests_support::Builder;
    use super::*;

    #[test]
    fn an_image_predating_the_inode_layout_is_refused() {
        let image = Builder::default().build();
        let mut old = image.clone();
        old[10..12].copy_from_slice(&0u16.to_le_bytes());
        let d = digest(&old[..Superblock::CHECKSUM_OFFSET]).to_le_bytes();
        old[Superblock::CHECKSUM_OFFSET..SUPERBLOCK_SIZE].copy_from_slice(&d);

        let err = match Image::new(old.clone(), 0, old.len() as u64) {
            Err(e) => e,
            Ok(_) => panic!("an image predating the layout change was accepted"),
        };
        assert!(
            matches!(
                err,
                Error::UnsupportedMinorVersion {
                    minor: 0,
                    supported: MIN_VERSION_MINOR
                }
            ),
            "expected a minor version refusal, got {err:?}"
        );
        // The message must name both versions so the cause is obvious.
        let text = err.to_string();
        assert!(
            text.contains("1.0"),
            "message should name the image's version: {text}"
        );
        assert!(
            text.contains(&format!("1.{MIN_VERSION_MINOR}")),
            "message should name ours: {text}"
        );
    }
}
