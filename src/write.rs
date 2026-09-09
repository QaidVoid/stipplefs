//! Building images.
//!
//! The logical stream and the encoded blocks live in a scratch file, and a
//! file added from disk is read only when its bytes are placed, so what the
//! writer holds does not grow with the size of the tree. [`Options`] carries a
//! ceiling on the blocks held while compressing; it does not cover the working
//! memory the compressor itself allocates for a given block size and level.
//! Output is byte-identical for identical input.

use crate::chunk::{ChunkerConfig, chunks};
use crate::error::{Error, Result};
use crate::format::{
    ALIGNMENT, ATTR_ENTRY_SIZE, BLOCK_ENTRY_SIZE, Codec, DIRENT_SIZE, EXTENT_SIZE,
    INODE_HEADER_SIZE, InodeWidths, MAGIC, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, S_IFBLK, S_IFCHR,
    S_IFDIR, S_IFIFO, S_IFLNK, S_IFREG, S_IFSOCK, SECTION_COUNT, SUPERBLOCK_SIZE, SectionKind,
    Superblock, VERSION_MAJOR, VERSION_MINOR,
};

use std::collections::{BTreeMap, HashMap};

/// How an image is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Decoded size of a full block.
    pub block_size: u32,
    /// Target average size of a deduplication chunk.
    pub chunk_average: u32,
    /// Codecs the writer may emit, as a bitmask.
    pub codecs: u16,
    /// Compression level, where the codec takes one.
    pub level: i32,
    /// Largest dictionary the writer will train, in bytes. Zero disables
    /// dictionary training.
    pub dictionary_size: usize,
    /// Worker threads used to compress blocks. Output does not depend on this.
    pub threads: usize,
    /// Rough ceiling on the memory used to compress blocks, in bytes. `None`
    /// lets every worker hold a block. Output does not depend on this.
    pub memory_limit: Option<usize>,
    /// What to record for modification times.
    pub timestamps: Timestamps,
}

/// What the writer records for modification times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timestamps {
    /// Record each entry's own time. Faithful, and reproducible for a given
    /// set of times.
    #[default]
    Preserve,
    /// Record one fixed value for every entry, so that trees differing only in
    /// their timestamps produce byte-identical images.
    Clamp,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            block_size: 65536,
            chunk_average: 65536,
            codecs: Codec::Stored.bit() | Codec::Zstd.bit() | Codec::ZstdDict.bit(),
            level: 19,
            dictionary_size: 112 * 1024,
            threads: 1,
            memory_limit: None,
            timestamps: Timestamps::default(),
        }
    }
}

/// What a name in the tree refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A directory.
    Directory {
        /// Permission bits, without the type.
        mode: u32,
        /// Owning user.
        uid: u32,
        /// Owning group.
        gid: u32,
    },
    /// A regular file.
    File {
        /// The file's bytes.
        content: Vec<u8>,
        /// Permission bits, without the type.
        mode: u32,
        /// Owning user.
        uid: u32,
        /// Owning group.
        gid: u32,
        /// Names sharing a link group become one inode, as hard links do.
        link_group: Option<u64>,
    },
    /// A symbolic link.
    Symlink {
        /// The target, stored verbatim.
        target: Vec<u8>,
        /// Owning user.
        uid: u32,
        /// Owning group.
        gid: u32,
    },
    /// A named pipe or a socket, neither of which carries content.
    Special {
        /// Which of the two.
        kind: SpecialKind,
        /// Permission bits, without the type.
        mode: u32,
        /// Owning user.
        uid: u32,
        /// Owning group.
        gid: u32,
    },
    /// A character or block device.
    Device {
        /// Which of the two.
        kind: DeviceKind,
        /// Device number, major in the upper half.
        rdev: u64,
        /// Permission bits, without the type.
        mode: u32,
        /// Owning user.
        uid: u32,
        /// Owning group.
        gid: u32,
    },
}

/// Entries that carry neither content nor a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKind {
    /// A named pipe.
    Fifo,
    /// A socket.
    Socket,
}

/// Entries that carry a device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// A character device.
    Char,
    /// A block device.
    Block,
}

#[derive(Default)]
struct Dir {
    children: BTreeMap<Vec<u8>, Tree>,
    mode: u32,
    uid: u32,
    gid: u32,
}

enum Tree {
    Dir(Dir),
    Node(Node),
    /// A file whose bytes stay on disk until they are placed, so that adding a
    /// tree does not hold every file in it in memory at once.
    #[cfg(feature = "fs")]
    FileAt {
        path: std::path::PathBuf,
        mode: u32,
        uid: u32,
        gid: u32,
        link_group: Option<u64>,
    },
}

/// A file whose content has not been placed into the stream yet.
///
/// Structure is decided in one pass and content in another, so that the order
/// content is placed in can differ from the order the tree is walked in.
enum Pending<'a> {
    Bytes(&'a [u8]),
    #[cfg(feature = "fs")]
    Path(&'a std::path::Path),
}

/// How many entries of each kind a builder holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Directories, including the root.
    pub directories: u64,
    /// Regular files, counting each name of a hard-linked file.
    pub files: u64,
    /// Symbolic links.
    pub symlinks: u64,
    /// Named pipes.
    pub fifos: u64,
    /// Sockets.
    pub sockets: u64,
    /// Character devices.
    pub char_devices: u64,
    /// Block devices.
    pub block_devices: u64,
}

impl Counts {
    /// Every kind and its count, in a fixed order suitable for reporting.
    pub fn by_kind(&self) -> [(&'static str, u64); 7] {
        [
            ("directories", self.directories),
            ("files", self.files),
            ("symlinks", self.symlinks),
            ("pipes", self.fifos),
            ("sockets", self.sockets),
            ("char devices", self.char_devices),
            ("block devices", self.block_devices),
        ]
    }
}

/// One extended attribute: a name and a value, both arbitrary bytes.
pub type Xattr = (Vec<u8>, Vec<u8>);

/// The set of extended attributes an entry carries, sorted by name.
pub type XattrSet = Vec<Xattr>;

/// Collects a tree and turns it into an image.
pub struct Builder {
    options: Options,
    root: Dir,
    /// Extended attributes, keyed by the same path used to insert the entry.
    /// Kept beside the tree rather than inside every node kind, since any kind
    /// may carry them and most carry none.
    xattrs: BTreeMap<Vec<u8>, XattrSet>,
    /// Modification times, keyed the same way. Every kind has one, so keeping
    /// them beside the tree avoids repeating a field on every node kind.
    mtimes: BTreeMap<Vec<u8>, u64>,
}

impl Builder {
    /// A builder with the given options.
    pub fn new(options: Options) -> Result<Self> {
        if !options.block_size.is_power_of_two()
            || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&options.block_size)
        {
            return Err(Error::Malformed {
                detail: "block_size is not a power of two within the permitted range",
            });
        }
        if options.codecs & Codec::Stored.bit() == 0 {
            return Err(Error::Malformed {
                detail: "the stored codec cannot be excluded",
            });
        }
        Ok(Builder {
            options,
            root: Dir {
                mode: 0o755,
                ..Dir::default()
            },
            xattrs: BTreeMap::new(),
            mtimes: BTreeMap::new(),
        })
    }

    /// Record a modification time for the entry at `path`, in seconds.
    ///
    /// Ignored when the writer is clamping times.
    pub fn set_mtime(&mut self, path: &[u8], seconds: u64) {
        let key: Vec<u8> = path
            .split(|&b| b == b'/')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(&b'/');
        self.mtimes.insert(key, seconds);
    }

    /// Attach extended attributes to the entry at `path`.
    ///
    /// Names are sorted and duplicates dropped, so the recorded order follows
    /// the content rather than the order they were supplied.
    pub fn set_xattrs(&mut self, path: &[u8], mut attrs: XattrSet) {
        if attrs.is_empty() {
            return;
        }
        attrs.sort();
        attrs.dedup_by(|a, b| a.0 == b.0);
        let key: Vec<u8> = path
            .split(|&b| b == b'/')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(&b'/');
        self.xattrs.insert(key, attrs);
    }

    /// Add a node at `path`, whose components are separated by `/`.
    ///
    /// Intermediate directories are created with default ownership if they
    /// have not been added already.
    pub fn insert(&mut self, path: &[u8], node: Node) -> Result<()> {
        let entry = match node {
            Node::Directory { mode, uid, gid } => Tree::Dir(Dir {
                children: BTreeMap::new(),
                mode,
                uid,
                gid,
            }),
            other => Tree::Node(other),
        };
        self.insert_tree(path, entry)
    }

    /// How many entries of each kind the builder holds.
    ///
    /// Counted before building, so a caller can report what went in without
    /// reading the image back.
    pub fn counts(&self) -> Counts {
        fn walk(dir: &Dir, counts: &mut Counts) {
            counts.directories += 1;
            for entry in dir.children.values() {
                match entry {
                    Tree::Dir(d) => walk(d, counts),
                    #[cfg(feature = "fs")]
                    Tree::FileAt { .. } => counts.files += 1,
                    Tree::Node(Node::File { .. }) => counts.files += 1,
                    Tree::Node(Node::Symlink { .. }) => counts.symlinks += 1,
                    Tree::Node(Node::Special {
                        kind: SpecialKind::Fifo,
                        ..
                    }) => counts.fifos += 1,
                    Tree::Node(Node::Special {
                        kind: SpecialKind::Socket,
                        ..
                    }) => counts.sockets += 1,
                    Tree::Node(Node::Device {
                        kind: DeviceKind::Char,
                        ..
                    }) => counts.char_devices += 1,
                    Tree::Node(Node::Device {
                        kind: DeviceKind::Block,
                        ..
                    }) => counts.block_devices += 1,
                    Tree::Node(Node::Directory { .. }) => {}
                }
            }
        }
        let mut counts = Counts::default();
        walk(&self.root, &mut counts);
        counts
    }

    fn insert_tree(&mut self, path: &[u8], entry: Tree) -> Result<()> {
        let parts: Vec<&[u8]> = path
            .split(|&b| b == b'/')
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return Err(Error::Malformed {
                detail: "cannot insert a node at the root",
            });
        }
        if parts.iter().any(|p| p.contains(&0)) {
            return Err(Error::Malformed {
                detail: "a name contains a zero byte",
            });
        }
        let mut dir = &mut self.root;
        for part in &parts[..parts.len() - 1] {
            let entry = dir.children.entry(part.to_vec()).or_insert_with(|| {
                Tree::Dir(Dir {
                    mode: 0o755,
                    ..Dir::default()
                })
            });
            match entry {
                Tree::Dir(d) => dir = d,
                _ => {
                    return Err(Error::Malformed {
                        detail: "a path component names something that is not a directory",
                    });
                }
            }
        }
        let last = parts[parts.len() - 1].to_vec();
        match (dir.children.get_mut(&last), &entry) {
            (Some(Tree::Dir(existing)), Tree::Dir(new)) => {
                existing.mode = new.mode;
                existing.uid = new.uid;
                existing.gid = new.gid;
                return Ok(());
            }
            _ => {
                dir.children.insert(last, entry);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct InodeRec {
    size: u64,
    payload: u64,
    extent_count: u32,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    mtime: u64,
    xattr: Option<u32>,
}

#[derive(Clone, Copy, Default)]
struct DirentRec {
    inode: u32,
    name_offset: u32,
    name_length: u16,
}

struct Plan {
    inodes: Vec<InodeRec>,
    dirents: Vec<DirentRec>,
    names: Vec<u8>,
    name_at: HashMap<Vec<u8>, u32>,
    extents: Vec<(u64, u64, u32)>,
    stream: Spill,
    seen: HashMap<u128, u64>,
    links: HashMap<u64, u32>,
    /// Distinct attribute sets, in order of first reference.
    xattr_sets: Vec<XattrSet>,
    xattr_index: HashMap<XattrSet, u32>,
}

impl Plan {
    fn new(stream: Spill) -> Self {
        Plan {
            inodes: Vec::new(),
            dirents: Vec::new(),
            names: Vec::new(),
            name_at: HashMap::new(),
            extents: Vec::new(),
            stream,
            seen: HashMap::new(),
            links: HashMap::new(),
            xattr_sets: Vec::new(),
            xattr_index: HashMap::new(),
        }
    }

    fn intern(&mut self, name: &[u8]) -> Result<u32> {
        if let Some(at) = self.name_at.get(name) {
            return Ok(*at);
        }
        let at = u32::try_from(self.names.len()).map_err(|_| Error::Malformed {
            detail: "the name table exceeds four gigabytes",
        })?;
        self.names.extend_from_slice(name);
        self.name_at.insert(name.to_vec(), at);
        Ok(at)
    }

    fn alloc(&mut self) -> u32 {
        self.inodes.push(InodeRec::default());
        (self.inodes.len() - 1) as u32
    }
}

impl Builder {
    /// Produce the image in memory.
    ///
    /// Use [`Builder::build_into`] for trees large enough that holding the
    /// finished image is itself the constraint.
    pub fn build(self) -> Result<Vec<u8>> {
        let mut image = Vec::new();
        self.build_into(&mut image)?;
        Ok(image)
    }

    /// Produce the image, writing it out as it is assembled.
    pub fn build_into(self, out: &mut dyn std::io::Write) -> Result<()> {
        let cfg = ChunkerConfig::new(self.options.chunk_average);
        let mut plan = Plan::new(Spill::create()?);
        let root = plan.alloc();
        let mut placements = Vec::new();
        self.visit(&self.root, root, &mut plan, &[], &mut placements)?;
        self.place_all(&placements, &mut plan, &cfg)?;

        let mut counts = vec![0u32; plan.inodes.len()];
        for d in &plan.dirents {
            counts[d.inode as usize] += 1;
        }
        for (i, rec) in plan.inodes.iter_mut().enumerate() {
            rec.nlink = if i == 0 { 2 } else { counts[i].max(1) };
        }

        self.emit(plan, out)
    }

    fn visit<'a>(
        &'a self,
        dir: &'a Dir,
        ino: u32,
        plan: &mut Plan,
        prefix: &[u8],
        placements: &mut Vec<(u32, Pending<'a>)>,
    ) -> Result<()> {
        let start = plan.dirents.len();
        plan.dirents
            .resize(start + dir.children.len(), DirentRec::default());
        plan.inodes[ino as usize] = InodeRec {
            size: dir.children.len() as u64,
            payload: start as u64,
            extent_count: 0,
            mode: S_IFDIR | (dir.mode & 0o7777),
            uid: dir.uid,
            gid: dir.gid,
            nlink: 0,
            mtime: 0,
            xattr: None,
        };

        for (i, (name, entry)) in dir.children.iter().enumerate() {
            let name_offset = plan.intern(name)?;
            let name_length = u16::try_from(name.len()).map_err(|_| Error::Malformed {
                detail: "a name is longer than sixty five thousand bytes",
            })?;

            let child = match entry {
                Tree::Node(Node::File {
                    link_group: Some(g),
                    ..
                }) if plan.links.contains_key(g) => plan.links[g],
                #[cfg(feature = "fs")]
                Tree::FileAt {
                    link_group: Some(g),
                    ..
                } if plan.links.contains_key(g) => plan.links[g],
                _ => plan.alloc(),
            };
            plan.dirents[start + i] = DirentRec {
                inode: child,
                name_offset,
                name_length,
            };

            match entry {
                Tree::Dir(d) => {
                    let mut nested = prefix.to_vec();
                    if !nested.is_empty() {
                        nested.push(b'/');
                    }
                    nested.extend_from_slice(name);
                    self.visit(d, child, plan, &nested, placements)?
                }
                Tree::Node(node) => self.record_node(child, node, plan, placements)?,
                #[cfg(feature = "fs")]
                Tree::FileAt {
                    path,
                    mode,
                    uid,
                    gid,
                    link_group,
                } => {
                    if plan.inodes[child as usize].mode == 0 {
                        if let Some(group) = link_group {
                            plan.links.insert(*group, child);
                        }
                        plan.inodes[child as usize] = InodeRec {
                            mode: S_IFREG | (mode & 0o7777),
                            uid: *uid,
                            gid: *gid,
                            ..InodeRec::default()
                        };
                        placements.push((child, Pending::Path(path)));
                    }
                }
            }

            let mut key = prefix.to_vec();
            if !key.is_empty() {
                key.push(b'/');
            }
            key.extend_from_slice(name);
            if self.options.timestamps == Timestamps::Preserve
                && let Some(t) = self.mtimes.get(&key)
            {
                plan.inodes[child as usize].mtime = *t;
            }
            if let Some(attrs) = self.xattrs.get(&key) {
                let at = match plan.xattr_index.get(attrs) {
                    Some(at) => *at,
                    None => {
                        let at = plan.xattr_sets.len() as u32;
                        plan.xattr_sets.push(attrs.clone());
                        plan.xattr_index.insert(attrs.clone(), at);
                        at
                    }
                };
                plan.inodes[child as usize].xattr = Some(at);
            }
        }
        Ok(())
    }

    fn record_node<'a>(
        &self,
        ino: u32,
        node: &'a Node,
        plan: &mut Plan,
        placements: &mut Vec<(u32, Pending<'a>)>,
    ) -> Result<()> {
        match node {
            Node::Directory { .. } => unreachable!("directories are visited, not emitted"),
            Node::Special {
                kind,
                mode,
                uid,
                gid,
            } => {
                let type_bits = match kind {
                    SpecialKind::Fifo => S_IFIFO,
                    SpecialKind::Socket => S_IFSOCK,
                };
                plan.inodes[ino as usize] = InodeRec {
                    size: 0,
                    payload: 0,
                    extent_count: 0,
                    mode: type_bits | (mode & 0o7777),
                    uid: *uid,
                    gid: *gid,
                    nlink: 0,
                    mtime: 0,
                    xattr: None,
                };
            }
            Node::Device {
                kind,
                rdev,
                mode,
                uid,
                gid,
            } => {
                let type_bits = match kind {
                    DeviceKind::Char => S_IFCHR,
                    DeviceKind::Block => S_IFBLK,
                };
                plan.inodes[ino as usize] = InodeRec {
                    size: 0,
                    payload: *rdev,
                    extent_count: 0,
                    mode: type_bits | (mode & 0o7777),
                    uid: *uid,
                    gid: *gid,
                    nlink: 0,
                    mtime: 0,
                    xattr: None,
                };
            }
            Node::Symlink { target, uid, gid } => {
                let at = plan.intern(target)?;
                plan.inodes[ino as usize] = InodeRec {
                    size: target.len() as u64,
                    payload: at as u64,
                    extent_count: 0,
                    mode: S_IFLNK | 0o777,
                    uid: *uid,
                    gid: *gid,
                    nlink: 0,
                    mtime: 0,
                    xattr: None,
                };
            }
            Node::File {
                content,
                mode,
                uid,
                gid,
                link_group,
            } => {
                if plan.inodes[ino as usize].mode != 0 {
                    return Ok(());
                }
                if let Some(g) = link_group {
                    plan.links.insert(*g, ino);
                }
                plan.inodes[ino as usize] = InodeRec {
                    size: 0,
                    payload: 0,
                    extent_count: 0,
                    mode: S_IFREG | (mode & 0o7777),
                    uid: *uid,
                    gid: *gid,
                    nlink: 0,
                    mtime: 0,
                    xattr: None,
                };
                placements.push((ino, Pending::Bytes(content)));
            }
        }
        Ok(())
    }

    /// Place every file's content into the stream, in the order given.
    fn place_all(
        &self,
        placements: &[(u32, Pending<'_>)],
        plan: &mut Plan,
        cfg: &ChunkerConfig,
    ) -> Result<()> {
        for (ino, pending) in placements {
            let content: std::borrow::Cow<'_, [u8]> = match pending {
                Pending::Bytes(bytes) => std::borrow::Cow::Borrowed(bytes),
                #[cfg(feature = "fs")]
                Pending::Path(path) => std::borrow::Cow::Owned(std::fs::read(path)?),
            };
            let (payload, extent_count) = self.place(&content, plan, cfg)?;
            let rec = &mut plan.inodes[*ino as usize];
            rec.size = content.len() as u64;
            rec.payload = payload;
            rec.extent_count = extent_count;
        }
        Ok(())
    }

    /// Chunk, deduplicate and place a file's content, returning its inode
    /// payload and extent count.
    fn place(&self, content: &[u8], plan: &mut Plan, cfg: &ChunkerConfig) -> Result<(u64, u32)> {
        if content.is_empty() {
            return Ok((0, 0));
        }
        let mut runs: Vec<(u64, u64, u32)> = Vec::new();
        let mut file_offset = 0u64;
        for (off, len) in chunks(content, cfg) {
            let piece = &content[off..off + len];
            let key = twox_hash::XxHash3_128::oneshot(piece);
            let at = match plan.seen.get(&key) {
                Some(at) => *at,
                None => {
                    let at = plan.stream.append(piece)?;
                    plan.seen.insert(key, at);
                    at
                }
            };
            match runs.last_mut() {
                Some(last) if last.0 + last.2 as u64 == at => {
                    last.2 += len as u32;
                }
                _ => runs.push((at, file_offset, len as u32)),
            }
            file_offset += len as u64;
        }
        if runs.len() == 1 {
            return Ok((runs[0].0, 1));
        }
        let first = plan.extents.len() as u64;
        plan.extents.extend(runs.iter().copied());
        Ok((first, (plan.extents.len() as u64 - first) as u32))
    }
}

/// A scratch file holding the logical stream, and then the encoded blocks,
/// while an image is built.
///
/// Both are written once and read back once, which is what keeps the writer's
/// memory independent of the size of the tree being packed. The file is
/// unlinked as soon as it is open, so it costs nothing if the process dies.
struct Spill {
    file: std::fs::File,
    buffer: Vec<u8>,
    len: u64,
}

impl Spill {
    const BUFFER: usize = 1 << 20;

    fn create() -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let directory = std::env::temp_dir();
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("stipplefs-{}-{nonce}.tmp", std::process::id()));
        let fail = |e: std::io::Error| Error::Temporary {
            directory: directory.display().to_string(),
            detail: e.to_string(),
        };
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(fail)?;
        std::fs::remove_file(&path).map_err(fail)?;
        Ok(Spill {
            file,
            buffer: Vec::with_capacity(Self::BUFFER),
            len: 0,
        })
    }

    /// Append `bytes` and return the offset they were written at.
    fn append(&mut self, bytes: &[u8]) -> Result<u64> {
        use std::io::Write;
        let at = self.len;
        if self.buffer.len() + bytes.len() > Self::BUFFER {
            self.flush()?;
        }
        if bytes.len() >= Self::BUFFER {
            self.file.write_all(bytes)?;
        } else {
            self.buffer.extend_from_slice(bytes);
        }
        self.len += bytes.len() as u64;
        Ok(at)
    }

    fn flush(&mut self) -> Result<()> {
        use std::io::Write;
        if !self.buffer.is_empty() {
            self.file.write_all(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }

    /// Fill `buf` from `at`. Everything read must already have been flushed.
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.read_exact_at(buf, at)?;
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

/// Where one compressed block ended up in the scratch file, and what the
/// block index has to say about it.
struct EncodedBlock {
    codec: Codec,
    offset: u64,
    encoded_length: u32,
    decoded_length: u32,
    digest: u64,
}

fn write_zeros(out: &mut dyn std::io::Write, mut n: u64) -> Result<()> {
    const ZEROS: [u8; 1024] = [0u8; 1024];
    while n > 0 {
        let take = (ZEROS.len() as u64).min(n) as usize;
        out.write_all(&ZEROS[..take])?;
        n -= take as u64;
    }
    Ok(())
}

impl Builder {
    /// Choose how to store one block.
    ///
    /// Stored wins unless another permitted codec is strictly smaller, and
    /// among equally sized results the lowest codec identifier wins, so the
    /// choice is decided by content rather than by iteration order.
    fn encode_block(&self, raw: &[u8], dict: Option<&[u8]>) -> (Codec, Vec<u8>) {
        #[allow(unused_mut)]
        let mut best = (Codec::Stored, raw.to_vec());
        #[cfg(feature = "zstd")]
        if self.options.codecs & Codec::Zstd.bit() != 0
            && let Ok(z) = zstd::encode_all(raw, self.options.level)
            && z.len() < best.1.len()
        {
            best = (Codec::Zstd, z);
        }
        #[cfg(feature = "zstd")]
        if self.options.codecs & Codec::ZstdDict.bit() != 0
            && let Some(d) = dict
            && let Ok(mut c) = zstd::bulk::Compressor::with_dictionary(self.options.level, d)
            && let Ok(z) = c.compress(raw)
            && z.len() < best.1.len()
        {
            best = (Codec::ZstdDict, z);
        }
        #[cfg(not(feature = "zstd"))]
        let _ = dict;
        best
    }

    /// Compress every block, optionally across several threads.
    ///
    /// Each worker takes a contiguous run and results are reassembled in index
    /// order, so the output does not depend on the thread count or on which
    /// worker finishes first.
    fn encode_blocks(&self, blocks: &[&[u8]], dict: Option<&[u8]>) -> Vec<(Codec, Vec<u8>)> {
        let threads = self.options.threads.clamp(1, blocks.len().max(1));
        if threads == 1 {
            return blocks.iter().map(|b| self.encode_block(b, dict)).collect();
        }
        let per = blocks.len().div_ceil(threads);
        let parts: Vec<&[&[u8]]> = blocks.chunks(per).collect();
        let mut collected: Vec<Vec<(Codec, Vec<u8>)>> = Vec::with_capacity(parts.len());
        std::thread::scope(|scope| {
            let handles: Vec<_> = parts
                .iter()
                .map(|part| {
                    scope.spawn(move || {
                        part.iter()
                            .map(|b| self.encode_block(b, dict))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for h in handles {
                match h.join() {
                    Ok(v) => collected.push(v),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        });
        collected.into_iter().flatten().collect()
    }

    /// Train a dictionary from evenly spaced blocks.
    ///
    /// Samples are taken from an ordered sequence at a fixed stride and the
    /// parameters are pinned, so training is a pure function of the content.
    /// Selecting samples from a hash container here would reintroduce exactly
    /// the non-determinism the rest of the writer avoids.
    #[allow(unused_variables)]
    fn train_dictionary(
        &self,
        stream: &Spill,
        block_count: usize,
        block_size: usize,
    ) -> Result<Option<Vec<u8>>> {
        #[cfg(feature = "dict")]
        {
            if self.options.dictionary_size == 0
                || self.options.codecs & Codec::ZstdDict.bit() == 0
                || block_count < 8
            {
                return Ok(None);
            }
            const TARGET_SAMPLES: usize = 128;
            const SAMPLE_BYTES: usize = 256 * 1024;
            let stride = block_count.div_ceil(TARGET_SAMPLES).max(1);
            let width = block_size.min(SAMPLE_BYTES);
            let mut samples: Vec<Vec<u8>> = Vec::new();
            let mut index = 0;
            while index < block_count {
                let at = (index * block_size) as u64;
                let len = (width as u64).min(stream.len - at) as usize;
                let mut b = vec![0u8; len];
                stream.read_at(at, &mut b)?;
                samples.push(b);
                index += stride;
            }
            if samples.len() < 8 {
                return Ok(None);
            }
            let refs: Vec<&[u8]> = samples.iter().map(|b| b.as_slice()).collect();
            Ok(zstd::dict::from_samples(&refs, self.options.dictionary_size).ok())
        }
        #[cfg(not(feature = "dict"))]
        Ok(None)
    }

    fn emit(&self, mut plan: Plan, out: &mut dyn std::io::Write) -> Result<()> {
        let (inodes, attributes) = encode_inodes(&plan.inodes);
        let names = std::mem::take(&mut plan.names);
        let xattrs = encode_xattrs(&plan.xattr_sets);

        let mut dirents = Vec::with_capacity(plan.dirents.len() * DIRENT_SIZE);
        for d in &plan.dirents {
            dirents.extend_from_slice(&d.inode.to_le_bytes());
            dirents.extend_from_slice(&d.name_offset.to_le_bytes());
            dirents.extend_from_slice(&d.name_length.to_le_bytes());
            dirents.extend_from_slice(&0u16.to_le_bytes());
        }

        let mut extents = Vec::with_capacity(plan.extents.len() * EXTENT_SIZE);
        for (stream_offset, file_offset, length) in &plan.extents {
            extents.extend_from_slice(&stream_offset.to_le_bytes());
            extents.extend_from_slice(&file_offset.to_le_bytes());
            extents.extend_from_slice(&length.to_le_bytes());
            extents.extend_from_slice(&0u32.to_le_bytes());
        }

        let block_size = self.options.block_size as usize;
        plan.stream.flush()?;
        let stream_len = plan.stream.len;
        let block_count = (stream_len as usize).div_ceil(block_size.max(1));

        let dictionary = self.train_dictionary(&plan.stream, block_count, block_size)?;
        let blocks = self.encode_stream(
            &mut plan.stream,
            block_count,
            block_size,
            dictionary.as_deref(),
        )?;
        plan.stream.flush()?;
        let block_section_len = block_count * BLOCK_ENTRY_SIZE;

        let mut sections: Vec<(SectionKind, Vec<u8>)> = Vec::new();
        for (kind, content) in [
            (SectionKind::Inodes, inodes),
            (SectionKind::Dirents, dirents),
            (SectionKind::Names, names),
            (SectionKind::Extents, extents),
            (SectionKind::Attributes, attributes),
            (SectionKind::ExtendedAttributes, xattrs),
        ] {
            if !content.is_empty() {
                sections.push((kind, content));
            }
        }
        let dict_len = dictionary.as_ref().map_or(0, |d| d.len());

        let padded = |n: usize| -> u64 { ((n as u64 + 8).div_ceil(ALIGNMENT)) * ALIGNMENT };
        let mut data_at = ALIGNMENT;
        for (_, c) in &sections {
            data_at += padded(c.len());
        }
        if block_section_len > 0 {
            data_at += padded(block_section_len);
        }
        if dict_len > 0 {
            data_at += padded(dict_len);
        }

        let mut block_index = Vec::with_capacity(block_section_len);
        let mut at = data_at;
        let mut codec_mask = Codec::Stored.bit();
        for b in &blocks {
            codec_mask |= b.codec.bit();
            block_index.extend_from_slice(&at.to_le_bytes());
            block_index.extend_from_slice(&b.encoded_length.to_le_bytes());
            block_index.extend_from_slice(&b.decoded_length.to_le_bytes());
            block_index.push(b.codec.id());
            block_index.extend_from_slice(&[0u8; 3]);
            at += b.encoded_length as u64 + 8;
        }
        if block_section_len > 0 {
            sections.push((SectionKind::Blocks, block_index));
        }
        if let Some(d) = dictionary {
            sections.push((SectionKind::Dictionaries, d));
        }

        let mut placed = [(0u64, 0u64); SECTION_COUNT];
        let mut cursor = ALIGNMENT;
        for (kind, content) in &sections {
            placed[*kind as usize] = (cursor, content.len() as u64 + 8);
            cursor += padded(content.len());
        }
        let block_bytes: u64 = blocks.iter().map(|b| b.encoded_length as u64 + 8).sum();
        let image_length = (data_at + block_bytes).div_ceil(ALIGNMENT) * ALIGNMENT;

        let mut sb = [0u8; SUPERBLOCK_SIZE];
        sb[0..8].copy_from_slice(&MAGIC);
        sb[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        sb[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
        sb[14..16].copy_from_slice(&codec_mask.to_le_bytes());
        sb[16..24].copy_from_slice(&image_length.to_le_bytes());
        sb[24..32].copy_from_slice(&stream_len.to_le_bytes());
        sb[32..36].copy_from_slice(&self.options.block_size.to_le_bytes());
        sb[36..40].copy_from_slice(&(plan.inodes.len() as u32).to_le_bytes());
        sb[40..44].copy_from_slice(&(plan.extents.len() as u32).to_le_bytes());
        sb[44..48].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
        sb[48..52].copy_from_slice(&0u32.to_le_bytes());
        for (i, (off, size)) in placed.iter().enumerate() {
            let base = 56 + i * 16;
            sb[base..base + 8].copy_from_slice(&off.to_le_bytes());
            sb[base + 8..base + 16].copy_from_slice(&size.to_le_bytes());
        }
        let d = digest(&sb[..Superblock::CHECKSUM_OFFSET]);
        sb[Superblock::CHECKSUM_OFFSET..].copy_from_slice(&d.to_le_bytes());

        out.write_all(&sb)?;
        let mut written = ALIGNMENT;
        for (_, content) in &sections {
            out.write_all(content)?;
            out.write_all(&digest(content).to_le_bytes())?;
            let end = written + padded(content.len());
            write_zeros(out, end - written - content.len() as u64 - 8)?;
            written = end;
        }
        let mut buf = vec![0u8; Spill::BUFFER.min(block_size.max(1))];
        for b in &blocks {
            let mut left = b.encoded_length as u64;
            let mut from = b.offset;
            while left > 0 {
                let take = (buf.len() as u64).min(left) as usize;
                plan.stream.read_at(from, &mut buf[..take])?;
                out.write_all(&buf[..take])?;
                from += take as u64;
                left -= take as u64;
            }
            out.write_all(&b.digest.to_le_bytes())?;
            written += b.encoded_length as u64 + 8;
        }
        write_zeros(out, image_length - written)?;
        Ok(())
    }

    /// Compress the stream one bounded batch of blocks at a time, appending
    /// each encoded block to the same scratch file.
    fn encode_stream(
        &self,
        stream: &mut Spill,
        block_count: usize,
        block_size: usize,
        dict: Option<&[u8]>,
    ) -> Result<Vec<EncodedBlock>> {
        let stream_len = stream.len;
        let batch = self.batch_size(block_size);
        let mut blocks = Vec::with_capacity(block_count);
        let mut index = 0;
        while index < block_count {
            let n = batch.min(block_count - index);
            let mut raw: Vec<Vec<u8>> = Vec::with_capacity(n);
            for k in 0..n {
                let at = ((index + k) * block_size) as u64;
                let len = (block_size as u64).min(stream_len - at) as usize;
                let mut b = vec![0u8; len];
                stream.read_at(at, &mut b)?;
                raw.push(b);
            }
            let refs: Vec<&[u8]> = raw.iter().map(|b| b.as_slice()).collect();
            let encoded = self.encode_blocks(&refs, dict);
            for ((codec, enc), decoded) in encoded.iter().zip(&raw) {
                blocks.push(EncodedBlock {
                    codec: *codec,
                    offset: stream.append(enc)?,
                    encoded_length: enc.len() as u32,
                    decoded_length: decoded.len() as u32,
                    digest: digest(enc),
                });
            }
            index += n;
        }
        Ok(blocks)
    }

    /// How many blocks to hold in memory at once.
    ///
    /// One block per worker is the fastest arrangement. A memory limit lowers
    /// that, but it cannot go below a single block, because a block is the
    /// unit compression works on.
    fn batch_size(&self, block_size: usize) -> usize {
        let threads = self.options.threads.max(1);
        match self.options.memory_limit {
            None => threads,
            Some(limit) => (limit / block_size.max(1) / 2).clamp(1, threads),
        }
    }
}

/// The extended attributes recorded on `path`, without following a symlink.
///
/// Returns an empty vector when the entry has none, or when the underlying
/// filesystem does not support them.
#[cfg(feature = "fs")]
fn read_xattrs(path: &std::path::Path) -> XattrSet {
    use std::os::unix::ffi::OsStrExt;

    let mut raw: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    raw.push(0);
    let c_path = raw.as_ptr() as *const libc::c_char;

    // SAFETY: `c_path` is a nul-terminated path that outlives both calls. The
    // first passes a null buffer to ask for the size, which is how the
    // interface is defined; the second passes a buffer of exactly that size.
    let needed = unsafe { libc::llistxattr(c_path, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }
    let mut names = vec![0u8; needed as usize];
    let got =
        unsafe { libc::llistxattr(c_path, names.as_mut_ptr() as *mut libc::c_char, names.len()) };
    if got <= 0 {
        return Vec::new();
    }
    names.truncate(got as usize);

    let mut out = Vec::new();
    for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let mut key: Vec<u8> = name.to_vec();
        key.push(0);
        let k = key.as_ptr() as *const libc::c_char;
        // SAFETY: same contract as above, for one named attribute.
        let size = unsafe { libc::lgetxattr(c_path, k, std::ptr::null_mut(), 0) };
        if size < 0 {
            continue;
        }
        let mut value = vec![0u8; size as usize];
        let got = unsafe {
            libc::lgetxattr(
                c_path,
                k,
                value.as_mut_ptr() as *mut libc::c_void,
                value.len(),
            )
        };
        if got < 0 {
            continue;
        }
        value.truncate(got as usize);
        out.push((name.to_vec(), value));
    }
    out
}

#[cfg(feature = "fs")]
impl Builder {
    /// Add every entry under `root`, in a canonical order.
    ///
    /// Directory entries are sorted before being visited, so the result does
    /// not depend on the order the underlying filesystem happens to return.
    /// Files with a link count above one are grouped by their source inode, so
    /// hard links in the source become hard links in the image.
    pub fn add_tree(&mut self, root: &std::path::Path) -> Result<()> {
        self.add_dir(root, &[])
    }

    fn add_dir(&mut self, dir: &std::path::Path, prefix: &[u8]) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|e| e.path())
            .collect();
        entries.sort();

        for path in entries {
            let name = match path.file_name() {
                Some(n) => n.as_bytes().to_vec(),
                None => continue,
            };
            let meta = std::fs::symlink_metadata(&path)?;
            let mode = meta.permissions().mode() & 0o7777;
            let found = read_xattrs(&path);
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());

            let mut full = prefix.to_vec();
            if !full.is_empty() {
                full.push(b'/');
            }
            full.extend_from_slice(&name);

            if meta.is_dir() {
                self.insert(
                    &full,
                    Node::Directory {
                        mode,
                        uid: meta.uid(),
                        gid: meta.gid(),
                    },
                )?;
                self.add_dir(&path, &full)?;
            } else if meta.is_symlink() {
                let target = std::fs::read_link(&path)?;
                self.insert(
                    &full,
                    Node::Symlink {
                        target: target.as_os_str().as_bytes().to_vec(),
                        uid: meta.uid(),
                        gid: meta.gid(),
                    },
                )?;
            } else if meta.is_file() {
                let link_group = (meta.nlink() > 1).then(|| meta.ino());
                self.insert_tree(
                    &full,
                    Tree::FileAt {
                        path: path.clone(),
                        mode,
                        uid: meta.uid(),
                        gid: meta.gid(),
                        link_group,
                    },
                )?;
            } else {
                // Anything else is recorded by its type bits, or the build
                // stops. Skipping silently is how pipes, sockets and device
                // nodes used to vanish without a word.
                use std::os::unix::fs::FileTypeExt;
                let t = meta.file_type();
                let node = if t.is_fifo() {
                    Node::Special {
                        kind: SpecialKind::Fifo,
                        mode,
                        uid: meta.uid(),
                        gid: meta.gid(),
                    }
                } else if t.is_socket() {
                    Node::Special {
                        kind: SpecialKind::Socket,
                        mode,
                        uid: meta.uid(),
                        gid: meta.gid(),
                    }
                } else if t.is_char_device() || t.is_block_device() {
                    Node::Device {
                        kind: if t.is_char_device() {
                            DeviceKind::Char
                        } else {
                            DeviceKind::Block
                        },
                        rdev: meta.rdev(),
                        mode,
                        uid: meta.uid(),
                        gid: meta.gid(),
                    }
                } else {
                    return Err(Error::Unrepresentable {
                        path: path.display().to_string(),
                        detail: "the format has no kind for this entry",
                    });
                };
                self.insert(&full, node)?;
            }
            if !found.is_empty() {
                self.set_xattrs(&full, found);
            }
            if let Some(t) = modified {
                self.set_mtime(&full, t);
            }
        }
        Ok(())
    }
}

/// Smallest number of bytes that represents every value in `values`.
fn width_for(values: impl Iterator<Item = u64>) -> u8 {
    let max = values.max().unwrap_or(0);
    let bits = 64 - max.leading_zeros();
    (bits.div_ceil(8)).max(1) as u8
}

/// Encode the inode arrays and the attribute table they index.
///
/// Each array's width is the smallest that holds every value it must carry, so
/// the widths follow from the content rather than from a fixed guess about how
/// large a tree might be.
fn encode_inodes(records: &[InodeRec]) -> (Vec<u8>, Vec<u8>) {
    if records.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Distinct (mode, uid, gid) tuples, sorted so the order follows content.
    let mut tuples: Vec<(u32, u32, u32)> = records.iter().map(|r| (r.mode, r.uid, r.gid)).collect();
    tuples.sort_unstable();
    tuples.dedup();
    let index_of: HashMap<(u32, u32, u32), u32> = tuples
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, i as u32))
        .collect();

    let mut attributes = Vec::with_capacity(tuples.len() * ATTR_ENTRY_SIZE);
    for (mode, uid, gid) in &tuples {
        attributes.extend_from_slice(&mode.to_le_bytes());
        attributes.extend_from_slice(&uid.to_le_bytes());
        attributes.extend_from_slice(&gid.to_le_bytes());
    }

    let times: Vec<u64> = records.iter().map(|r| r.mtime).collect();
    let recorded = times.iter().any(|t| *t != 0);
    let mtime_base = if recorded {
        times.iter().copied().min().unwrap_or(0)
    } else {
        0
    };
    let any_xattr = records.iter().any(|r| r.xattr.is_some());

    let widths = InodeWidths {
        size: width_for(records.iter().map(|r| r.size)),
        payload: width_for(records.iter().map(|r| r.payload)),
        attrs: width_for(std::iter::once(tuples.len().saturating_sub(1) as u64)),
        nlink: width_for(records.iter().map(|r| r.nlink as u64)),
        mtime: if recorded {
            width_for(times.iter().map(|t| t - mtime_base))
        } else {
            0
        },
        xattr: if any_xattr {
            width_for(records.iter().map(|r| r.xattr.map_or(0, |x| x as u64 + 1)))
        } else {
            0
        },
        extents: width_for(records.iter().map(|r| r.extent_count as u64)),
        mtime_base,
    };

    let mut out = Vec::with_capacity(INODE_HEADER_SIZE + widths.per_inode() * records.len());
    out.push(widths.size);
    out.push(widths.payload);
    out.push(widths.attrs);
    out.push(widths.nlink);
    out.push(widths.mtime);
    out.push(widths.xattr);
    out.push(widths.extents);
    out.push(0);
    out.extend_from_slice(&widths.mtime_base.to_le_bytes());

    let push = |value: u64, width: u8, out: &mut Vec<u8>| {
        if width > 0 {
            out.extend_from_slice(&value.to_le_bytes()[..width as usize]);
        }
    };
    for r in records {
        push(r.size, widths.size, &mut out);
    }
    for r in records {
        push(r.payload, widths.payload, &mut out);
    }
    for r in records {
        let at = index_of[&(r.mode, r.uid, r.gid)];
        push(at as u64, widths.attrs, &mut out);
    }
    for r in records {
        push(r.nlink as u64, widths.nlink, &mut out);
    }
    for r in records {
        push(r.mtime.saturating_sub(mtime_base), widths.mtime, &mut out);
    }
    for r in records {
        push(r.xattr.map_or(0, |x| x as u64 + 1), widths.xattr, &mut out);
    }
    for r in records {
        push(r.extent_count as u64, widths.extents, &mut out);
    }
    (out, attributes)
}

/// Encode the extended attribute section.
///
/// Names and values share one pool and a repeated one is stored once, so a
/// tree where every file carries the same attribute name pays for that name a
/// single time.
fn encode_xattrs(sets: &[XattrSet]) -> Vec<u8> {
    if sets.is_empty() {
        return Vec::new();
    }
    let mut pool: Vec<u8> = Vec::new();
    let mut at: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    let intern = |bytes: &[u8], pool: &mut Vec<u8>, at: &mut BTreeMap<Vec<u8>, u32>| -> u32 {
        if let Some(off) = at.get(bytes) {
            return *off;
        }
        let off = pool.len() as u32;
        pool.extend_from_slice(bytes);
        at.insert(bytes.to_vec(), off);
        off
    };

    let mut starts: Vec<u32> = Vec::with_capacity(sets.len() + 1);
    let mut pairs: Vec<u8> = Vec::new();
    let mut count = 0u32;
    for set in sets {
        starts.push(count);
        for (name, value) in set {
            let name_off = intern(name, &mut pool, &mut at);
            let value_off = intern(value, &mut pool, &mut at);
            pairs.extend_from_slice(&name_off.to_le_bytes());
            pairs.extend_from_slice(&(name.len() as u16).to_le_bytes());
            pairs.extend_from_slice(&value_off.to_le_bytes());
            pairs.extend_from_slice(&(value.len() as u16).to_le_bytes());
            count += 1;
        }
    }
    starts.push(count);

    let mut out = Vec::new();
    out.extend_from_slice(&(sets.len() as u32).to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for s in &starts {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(&pairs);
    out.extend_from_slice(&pool);
    out
}
