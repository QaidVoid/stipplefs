//! Building images.
//!
//! The writer holds the logical stream in memory, so it suits trees that fit
//! comfortably in RAM. Output is byte-identical for identical input.

use crate::chunk::{ChunkerConfig, chunks};
use crate::error::{Error, Result};
use crate::format::{
    ALIGNMENT, Codec, DIRENT_SIZE, EXTENT_SIZE, INODE_SIZE, MAGIC, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE,
    S_IFDIR, S_IFLNK, S_IFREG, SECTION_COUNT, SUPERBLOCK_SIZE, SectionKind, Superblock,
    VERSION_MAJOR, VERSION_MINOR,
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
}

/// Collects a tree and turns it into an image.
pub struct Builder {
    options: Options,
    root: Dir,
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
        })
    }

    /// Add a node at `path`, whose components are separated by `/`.
    ///
    /// Intermediate directories are created with default ownership if they
    /// have not been added already.
    pub fn insert(&mut self, path: &[u8], node: Node) -> Result<()> {
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
                Tree::Node(_) => {
                    return Err(Error::Malformed {
                        detail: "a path component names something that is not a directory",
                    });
                }
            }
        }
        let last = parts[parts.len() - 1].to_vec();
        let entry = match node {
            Node::Directory { mode, uid, gid } => Tree::Dir(Dir {
                children: BTreeMap::new(),
                mode,
                uid,
                gid,
            }),
            other => Tree::Node(other),
        };
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
}

#[derive(Clone, Copy, Default)]
struct DirentRec {
    inode: u32,
    name_offset: u32,
    name_length: u16,
}

#[derive(Default)]
struct Plan {
    inodes: Vec<InodeRec>,
    dirents: Vec<DirentRec>,
    names: Vec<u8>,
    name_at: HashMap<Vec<u8>, u32>,
    extents: Vec<(u64, u64, u32)>,
    stream: Vec<u8>,
    seen: HashMap<u128, u64>,
    links: HashMap<u64, u32>,
}

impl Plan {
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
    /// Produce the image.
    pub fn build(self) -> Result<Vec<u8>> {
        let cfg = ChunkerConfig::new(self.options.chunk_average);
        let mut plan = Plan::default();
        let root = plan.alloc();
        self.visit(&self.root, root, &mut plan, &cfg)?;

        let mut counts = vec![0u32; plan.inodes.len()];
        for d in &plan.dirents {
            counts[d.inode as usize] += 1;
        }
        for (i, rec) in plan.inodes.iter_mut().enumerate() {
            rec.nlink = if i == 0 { 2 } else { counts[i].max(1) };
        }

        self.emit(plan)
    }

    fn visit(&self, dir: &Dir, ino: u32, plan: &mut Plan, cfg: &ChunkerConfig) -> Result<()> {
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
                _ => plan.alloc(),
            };
            plan.dirents[start + i] = DirentRec {
                inode: child,
                name_offset,
                name_length,
            };

            match entry {
                Tree::Dir(d) => self.visit(d, child, plan, cfg)?,
                Tree::Node(node) => self.emit_node(child, node, plan, cfg)?,
            }
        }
        Ok(())
    }

    fn emit_node(&self, ino: u32, node: &Node, plan: &mut Plan, cfg: &ChunkerConfig) -> Result<()> {
        match node {
            Node::Directory { .. } => unreachable!("directories are visited, not emitted"),
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
                let (payload, extent_count) = self.place(content, plan, cfg)?;
                plan.inodes[ino as usize] = InodeRec {
                    size: content.len() as u64,
                    payload,
                    extent_count,
                    mode: S_IFREG | (mode & 0o7777),
                    uid: *uid,
                    gid: *gid,
                    nlink: 0,
                };
            }
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
                    let at = plan.stream.len() as u64;
                    plan.stream.extend_from_slice(piece);
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

fn digest(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

fn pad_to_alignment(v: &mut Vec<u8>) {
    let over = v.len() as u64 % ALIGNMENT;
    if over != 0 {
        v.resize(v.len() + (ALIGNMENT - over) as usize, 0);
    }
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
    fn train_dictionary(&self, blocks: &[&[u8]]) -> Option<Vec<u8>> {
        #[cfg(feature = "dict")]
        {
            if self.options.dictionary_size == 0
                || self.options.codecs & Codec::ZstdDict.bit() == 0
                || blocks.len() < 8
            {
                return None;
            }
            const TARGET_SAMPLES: usize = 128;
            let stride = blocks.len().div_ceil(TARGET_SAMPLES).max(1);
            let samples: Vec<&[u8]> = blocks.iter().step_by(stride).copied().collect();
            if samples.len() < 8 {
                return None;
            }
            zstd::dict::from_samples(&samples, self.options.dictionary_size).ok()
        }
        #[cfg(not(feature = "dict"))]
        None
    }

    fn emit(&self, plan: Plan) -> Result<Vec<u8>> {
        let mut inodes = Vec::with_capacity(plan.inodes.len() * INODE_SIZE);
        for r in &plan.inodes {
            inodes.extend_from_slice(&r.size.to_le_bytes());
            inodes.extend_from_slice(&r.payload.to_le_bytes());
            inodes.extend_from_slice(&r.extent_count.to_le_bytes());
            inodes.extend_from_slice(&r.mode.to_le_bytes());
            inodes.extend_from_slice(&r.uid.to_le_bytes());
            inodes.extend_from_slice(&r.gid.to_le_bytes());
            inodes.extend_from_slice(&r.nlink.to_le_bytes());
            inodes.extend_from_slice(&0u32.to_le_bytes());
        }

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
        let raw_blocks: Vec<&[u8]> = plan.stream.chunks(block_size).collect();
        let dictionary = self.train_dictionary(&raw_blocks);
        let encoded = self.encode_blocks(&raw_blocks, dictionary.as_deref());
        let block_section_len = encoded.len() * 16;

        let mut sections: Vec<(SectionKind, Vec<u8>)> = Vec::new();
        for (kind, content) in [
            (SectionKind::Inodes, inodes),
            (SectionKind::Dirents, dirents),
            (SectionKind::Names, plan.names),
            (SectionKind::Extents, extents),
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
        for ((codec, enc), raw) in encoded.iter().zip(&raw_blocks) {
            codec_mask |= codec.bit();
            block_index.extend_from_slice(&at.to_le_bytes());
            block_index.extend_from_slice(&(enc.len() as u32).to_le_bytes());
            block_index.extend_from_slice(&(raw.len() as u32).to_le_bytes()[..3]);
            block_index.push(codec.id());
            at += enc.len() as u64 + 8;
        }
        if block_section_len > 0 {
            sections.push((SectionKind::Blocks, block_index));
        }
        if let Some(d) = dictionary {
            sections.push((SectionKind::Dictionaries, d));
        }

        let mut body = Vec::new();
        let mut placed = [(0u64, 0u64); SECTION_COUNT];
        let mut cursor = ALIGNMENT;
        for (kind, content) in &sections {
            let size = content.len() as u64 + 8;
            placed[*kind as usize] = (cursor, size);
            body.extend_from_slice(content);
            body.extend_from_slice(&digest(content).to_le_bytes());
            pad_to_alignment(&mut body);
            cursor = ALIGNMENT + body.len() as u64;
        }
        for (_, enc) in &encoded {
            body.extend_from_slice(enc);
            body.extend_from_slice(&digest(enc).to_le_bytes());
        }
        pad_to_alignment(&mut body);

        let image_length = ALIGNMENT + body.len() as u64;

        let mut sb = [0u8; SUPERBLOCK_SIZE];
        sb[0..8].copy_from_slice(&MAGIC);
        sb[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        sb[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
        sb[14..16].copy_from_slice(&codec_mask.to_le_bytes());
        sb[16..24].copy_from_slice(&image_length.to_le_bytes());
        sb[24..32].copy_from_slice(&(plan.stream.len() as u64).to_le_bytes());
        sb[32..36].copy_from_slice(&self.options.block_size.to_le_bytes());
        sb[36..40].copy_from_slice(&(plan.inodes.len() as u32).to_le_bytes());
        sb[40..44].copy_from_slice(&(plan.extents.len() as u32).to_le_bytes());
        sb[44..48].copy_from_slice(&(encoded.len() as u32).to_le_bytes());
        sb[48..52].copy_from_slice(&0u32.to_le_bytes());
        for (i, (off, size)) in placed.iter().enumerate() {
            let base = 56 + i * 16;
            sb[base..base + 8].copy_from_slice(&off.to_le_bytes());
            sb[base + 8..base + 16].copy_from_slice(&size.to_le_bytes());
        }
        let d = digest(&sb[..Superblock::CHECKSUM_OFFSET]);
        sb[Superblock::CHECKSUM_OFFSET..].copy_from_slice(&d.to_le_bytes());

        let mut image = sb.to_vec();
        image.extend_from_slice(&body);
        Ok(image)
    }
}
