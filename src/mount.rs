//! Exposing an image through FUSE.
//!
//! The mount is read only. An entry whose image records no modification time
//! is reported as the Unix epoch, which is what an image built for
//! reproducibility holds for every entry.

use crate::format::{FileType as OurType, Inode};
use crate::read::Reader;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, OpenFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, ReplyXattr, Request,
};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

/// How long the kernel may trust an attribute or lookup result.
///
/// An image never changes while mounted, so entries never need revalidating.
const FOREVER: Duration = Duration::from_secs(60 * 60 * 24 * 365);

/// A mounted image.
pub struct Mount {
    reader: &'static Reader<'static>,
    warm: Option<&'static Warm>,
}

/// Largest single store into the kernel's page cache, a multiple of the page
/// size.
const PUSH_CHUNK: usize = 1 << 20;

/// The granularity the kernel marks cached file bytes valid at.
const PAGE: u64 = 4096;

/// Blocks waiting to be warmed.
///
/// A block some read or open is waiting on goes ahead of one warmed in case it
/// is wanted, or the application stalls behind speculation.
#[derive(Default)]
struct Pending {
    urgent: VecDeque<u32>,
    later: VecDeque<u32>,
}

/// Part of a page that straddles two pieces, held until the rest arrives.
struct Edge {
    bytes: Vec<u8>,
    filled: u64,
}

/// The part of one file that lies inside one block.
#[derive(Clone, Copy)]
struct Piece {
    block: u32,
    ino: u32,
    within: u32,
    len: u32,
    file_offset: u64,
}

/// Decodes blocks on background threads and hands their file bytes to the
/// kernel's page cache.
///
/// A large block costs far more to decode than any read it serves, and an
/// application that maps a library faults its pages in at scattered offsets
/// for as long as it runs. Holding every block such faults might reach takes
/// memory in proportion to the whole working set, and dropping them means
/// decoding the same block again and again. Instead, once a block is decoded
/// every file byte it holds is stored in the page cache, so no later read
/// needs the block and it can be dropped at once. Each block is decoded once,
/// and the memory it lands in is page cache, which the kernel reclaims under
/// pressure; a reclaimed page is simply read again.
///
/// The kernel accepts a store only for an inode it holds. Before storing a
/// block, the files in it that the kernel has not looked up are looked up
/// through the mountpoint.
///
/// Blocks are queued when a read touches them, when the application opens a
/// file whose bytes they hold, for a few blocks past each read, and for the
/// front of the stream at mount. A hot list places what an application reads
/// first at the front, in the order it reads it, so those are the blocks it
/// wants next.
struct Warm {
    reader: &'static Reader<'static>,
    notifier: Arc<OnceLock<fuser::Notifier>>,
    mountpoint: PathBuf,
    pieces: Vec<Piece>,
    /// Where each block's pieces begin in `pieces`, one entry past the end.
    by_block: Vec<u32>,
    /// Indices into `pieces`, grouped by inode.
    file_pieces: Vec<u32>,
    /// Where each inode's entries begin in `file_pieces`.
    by_file: Vec<u32>,
    /// Each inode's parent and directory entry index, for building its path.
    parent: Vec<Option<(u32, u32)>>,
    known: Vec<AtomicBool>,
    pushed: Vec<AtomicBool>,
    queued: Vec<AtomicBool>,
    done: Vec<AtomicBool>,
    pending: Mutex<Pending>,
    wake: Condvar,
    depth: u32,
    /// Pages a piece covers only part of, keyed by inode and page index.
    edges: Mutex<HashMap<(u32, u64), Edge>>,
}

impl Warm {
    fn new(reader: &'static Reader<'static>, mountpoint: PathBuf, depth: u32) -> Self {
        let block_size = reader.superblock().block_size as u64;
        let inodes = reader.superblock().inode_count as usize;
        let blocks = reader.blocks().count() as usize;

        let mut parent = vec![None; inodes];
        let mut dirs = vec![reader.root()];
        while let Some(dir) = dirs.pop() {
            let Some(node) = reader.inode(dir) else {
                continue;
            };
            for i in 0..node.size as u32 {
                let at = node.payload as u32 + i;
                let Some(entry) = reader.dirent(at) else {
                    continue;
                };
                let child = entry.inode as usize;
                if child < inodes && parent[child].is_none() && entry.inode != reader.root() {
                    parent[child] = Some((dir, at));
                    if reader.inode(entry.inode).and_then(|n| n.file_type())
                        == Some(OurType::Directory)
                    {
                        dirs.push(entry.inode);
                    }
                }
            }
        }

        let mut found = Vec::new();
        let mut by_file = Vec::with_capacity(inodes + 1);
        for ino in 0..inodes as u32 {
            by_file.push(found.len() as u32);
            for (mut stream, mut file_offset, mut left) in reader.file_extents(ino) {
                while left > 0 {
                    let within = stream % block_size;
                    let len = left.min(block_size - within);
                    found.push(Piece {
                        block: (stream / block_size) as u32,
                        ino,
                        within: within as u32,
                        len: len as u32,
                        file_offset,
                    });
                    stream += len;
                    file_offset += len;
                    left -= len;
                }
            }
        }
        by_file.push(found.len() as u32);

        let mut by_block = vec![0u32; blocks + 1];
        for p in &found {
            by_block[p.block as usize + 1] += 1;
        }
        for i in 0..blocks {
            by_block[i + 1] += by_block[i];
        }
        let mut next = by_block.clone();
        let mut pieces = found.clone();
        let mut file_pieces = vec![0u32; found.len()];
        for (i, p) in found.iter().enumerate() {
            let at = &mut next[p.block as usize];
            pieces[*at as usize] = *p;
            file_pieces[i] = *at;
            *at += 1;
        }

        let flags = |n: usize| (0..n).map(|_| AtomicBool::new(false)).collect();
        Warm {
            reader,
            notifier: Arc::new(OnceLock::new()),
            mountpoint,
            known: flags(inodes),
            pushed: flags(pieces.len()),
            queued: flags(blocks),
            done: flags(blocks),
            pieces,
            by_block,
            file_pieces,
            by_file,
            parent,
            pending: Mutex::new(Pending::default()),
            wake: Condvar::new(),
            depth,
            edges: Mutex::new(HashMap::new()),
        }
    }

    /// Queue a block for warming, unless it is warm or already queued. An
    /// urgent request moves a block queued for later to the front.
    fn queue(&self, block: u32, urgent: bool) {
        let i = block as usize;
        if i >= self.done.len() || self.done[i].load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if !self.queued[i].swap(true, Ordering::Relaxed) {
            if urgent {
                pending.urgent.push_back(block);
            } else {
                pending.later.push_back(block);
            }
        } else if urgent && let Some(at) = pending.later.iter().position(|&b| b == block) {
            pending.later.remove(at);
            pending.urgent.push_back(block);
        } else {
            return;
        }
        drop(pending);
        self.wake.notify_one();
    }

    /// The next block to warm, waiting until there is one.
    fn next(&self) -> Option<u32> {
        let mut pending = self.pending.lock().ok()?;
        loop {
            if let Some(b) = pending
                .urgent
                .pop_front()
                .or_else(|| pending.later.pop_front())
            {
                return Some(b);
            }
            pending = self.wake.wait(pending).ok()?;
        }
    }

    /// Queue the blocks a read touched and the few after them.
    fn after_read(&self, ino: u32, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        let first = self.reader.block_of(ino, offset);
        let last = self.reader.block_of(ino, offset + len - 1);
        for b in [first, last].into_iter().flatten() {
            self.queue(b, true);
        }
        if let Some(b) = last {
            for k in 1..=self.depth {
                self.queue(b + k, false);
            }
        }
    }

    /// Note that the kernel holds `ino`.
    fn looked_up(&self, ino: u32) {
        if let Some(k) = self.known.get(ino as usize) {
            k.store(true, Ordering::Relaxed);
        }
    }

    /// Queue the blocks holding what is not yet stored of a file the
    /// application opened.
    ///
    /// An open rather than a lookup, because looking a file up says little:
    /// a language runtime probing for modules looks up far more files than it
    /// reads.
    fn opened(&self, ino: u32) {
        if ino as usize >= self.known.len() {
            return;
        }
        let (from, to) = (self.by_file[ino as usize], self.by_file[ino as usize + 1]);
        for &i in &self.file_pieces[from as usize..to as usize] {
            if !self.pushed[i as usize].load(Ordering::Relaxed) {
                self.queue(self.pieces[i as usize].block, true);
            }
        }
    }

    /// The path of `ino` below the mountpoint.
    fn path(&self, mut ino: u32) -> Option<PathBuf> {
        let mut parts = Vec::new();
        while let Some((dir, at)) = self.parent.get(ino as usize).copied().flatten() {
            let entry = self.reader.dirent(at)?;
            parts.push(OsStr::from_bytes(self.reader.name(entry)?));
            ino = dir;
        }
        let mut path = self.mountpoint.clone();
        path.extend(parts.iter().rev());
        Some(path)
    }

    /// Decode a block and store every file byte it holds.
    fn warm(&self, block: u32) {
        self.warm_once(block);
        // Cleared only now, so a block cannot be queued again while it is
        // being warmed.
        self.queued[block as usize].store(false, Ordering::Relaxed);
    }

    fn warm_once(&self, block: u32) {
        let i = block as usize;
        if self.done[i].load(Ordering::Relaxed) {
            return;
        }
        // Blocks queued at mount can arrive before the session exists.
        let mut waited = 0;
        while self.notifier.get().is_none() {
            if waited == 1000 {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
            waited += 1;
        }
        let Ok(bytes) = self.reader.block_bytes(block) else {
            return;
        };
        let range = self.by_block[i] as usize..self.by_block[i + 1] as usize;
        for p in &self.pieces[range.clone()] {
            if !self.known[p.ino as usize].load(Ordering::Relaxed)
                && let Some(path) = self.path(p.ino)
            {
                let _ = std::fs::symlink_metadata(path);
            }
        }
        let mut all = true;
        for piece in range {
            all &= self.store(piece, &bytes);
        }
        if all {
            self.done[i].store(true, Ordering::Relaxed);
            // Every byte is in the page cache now, so no read needs it.
            drop(bytes);
            self.reader.forget_block(block);
        }
    }

    /// Store one piece from its decoded block, saying whether it is stored.
    ///
    /// The kernel marks a cached page valid only when one store covers it from
    /// its start to its end or to the end of the file. A piece rarely begins
    /// or ends on a page boundary, so the pages it shares with its neighbours
    /// are collected in `edges` and stored once complete.
    fn store(&self, i: usize, block: &[u8]) -> bool {
        let piece = self.pieces[i];
        if self.pushed[i].load(Ordering::Relaxed) {
            return true;
        }
        if !self.known[piece.ino as usize].load(Ordering::Relaxed) {
            return false;
        }
        let Some(notifier) = self.notifier.get() else {
            return false;
        };
        let Some(size) = self.reader.inode(piece.ino).map(|n| n.size) else {
            return false;
        };
        let start = piece.within as usize;
        let Some(bytes) = block.get(start..start + piece.len as usize) else {
            return false;
        };
        let (from, to) = (piece.file_offset, piece.file_offset + piece.len as u64);
        let head = from.next_multiple_of(PAGE).min(to);
        let tail = if to == size {
            to
        } else {
            (to / PAGE * PAGE).max(head)
        };
        let ino = Mount::to_kernel(piece.ino);

        let mut offset = head;
        let interior = &bytes[(head - from) as usize..(tail - from) as usize];
        for chunk in interior.chunks(PUSH_CHUNK) {
            if notifier.store(ino, offset, chunk).is_err() {
                return false;
            }
            offset += chunk.len() as u64;
        }
        for (a, b) in [(from, head), (tail, to)] {
            if a < b
                && !self.edge(
                    notifier,
                    piece.ino,
                    size,
                    a,
                    &bytes[(a - from) as usize..(b - from) as usize],
                )
            {
                return false;
            }
        }
        self.pushed[i].store(true, Ordering::Relaxed);
        true
    }
}

impl Warm {
    /// Add the bytes at `offset` to the partial page holding them, and store
    /// the page once every byte of it has arrived.
    fn edge(
        &self,
        notifier: &fuser::Notifier,
        ino: u32,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> bool {
        let page = offset / PAGE;
        let page_start = page * PAGE;
        let page_len = (size - page_start).min(PAGE);
        let Ok(mut edges) = self.edges.lock() else {
            return false;
        };
        let edge = edges.entry((ino, page)).or_insert_with(|| Edge {
            bytes: vec![0; page_len as usize],
            filled: 0,
        });
        let at = (offset - page_start) as usize;
        edge.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        edge.filled += bytes.len() as u64;
        if edge.filled < page_len {
            return true;
        }
        let Some(done) = edges.remove(&(ino, page)) else {
            return false;
        };
        drop(edges);
        notifier
            .store(Mount::to_kernel(ino), page_start, &done.bytes)
            .is_ok()
    }
}

impl Mount {
    /// Take ownership of an image and serve it through FUSE.
    ///
    /// The image and its reader are leaked on purpose. FUSE requires the
    /// filesystem to be `'static`, a mount lives for as long as the process
    /// does, and leaking two allocations is preferable to a self-referential
    /// struct or to extending a lifetime unsafely.
    pub fn new(image: crate::read::Image<'static>, budget: usize) -> crate::error::Result<Mount> {
        let image: &'static crate::read::Image<'static> = Box::leak(Box::new(image));
        let reader: &'static Reader<'static> =
            Box::leak(Box::new(image.reader_with_budget(budget)?));
        Ok(Mount { reader, warm: None })
    }

    /// Decode blocks on `workers` background threads and store their file
    /// bytes in the kernel's page cache, reaching `depth` blocks past each
    /// read and warming the first `start` blocks of the stream at once.
    ///
    /// A large block costs far more to decode than any read it serves, and
    /// keeping enough of them cached to serve a starting application takes
    /// memory in proportion to everything it touches. Once a block's file
    /// bytes are in the page cache, which the kernel can reclaim, no read
    /// needs the block again, so each is decoded once and the reader's cache
    /// can stay small.
    ///
    /// `mountpoint` is where the session will be mounted, since warming looks
    /// files up through it. The kernel accepts stores only once the session
    /// exists, so the caller puts the session's notifier into the returned
    /// cell; until then nothing is warmed and reads are served as usual.
    pub fn with_warming(
        mut self,
        mountpoint: &Path,
        workers: usize,
        depth: u32,
        start: u32,
    ) -> (Mount, Arc<OnceLock<fuser::Notifier>>) {
        let warm: &'static Warm = Box::leak(Box::new(Warm::new(
            self.reader,
            mountpoint.to_path_buf(),
            depth,
        )));
        for _ in 0..workers.max(1) {
            std::thread::spawn(move || {
                while let Some(block) = warm.next() {
                    warm.warm(block);
                }
            });
        }
        for block in 0..start {
            warm.queue(block, false);
        }
        self.warm = Some(warm);
        (self, Arc::clone(&warm.notifier))
    }

    /// The reader behind this mount, so a caller can read its counters.
    pub fn reader(&self) -> &'static Reader<'static> {
        self.reader
    }

    /// The kernel numbers the root 1, while the format numbers it 0.
    fn to_image(ino: INodeNo) -> Option<u32> {
        u32::try_from(ino.0.checked_sub(1)?).ok()
    }

    fn to_kernel(ino: u32) -> INodeNo {
        INodeNo(ino as u64 + 1)
    }

    /// The kernel's name for what an inode describes.
    fn kind_of(node: &Inode) -> FileType {
        match node.file_type() {
            Some(OurType::Directory) => FileType::Directory,
            Some(OurType::Symlink) => FileType::Symlink,
            Some(OurType::Fifo) => FileType::NamedPipe,
            Some(OurType::Socket) => FileType::Socket,
            Some(OurType::CharDevice) => FileType::CharDevice,
            Some(OurType::BlockDevice) => FileType::BlockDevice,
            _ => FileType::RegularFile,
        }
    }

    /// Answer an extended attribute request.
    ///
    /// A caller asks with a size of zero to learn how much room it needs, then
    /// asks again with a buffer that large.
    fn reply_xattr(value: &[u8], size: u32, reply: ReplyXattr) {
        if size == 0 {
            reply.size(value.len() as u32);
        } else if (size as usize) < value.len() {
            reply.error(Errno::ERANGE);
        } else {
            reply.data(value);
        }
    }

    fn attr(&self, ino: u32, node: &Inode) -> FileAttr {
        let kind = Self::kind_of(node);
        let size = match node.file_type() {
            Some(OurType::Directory) => 0,
            _ => node.size,
        };
        let mtime = node
            .mtime
            .and_then(|t| UNIX_EPOCH.checked_add(Duration::from_secs(t)))
            .unwrap_or(UNIX_EPOCH);
        FileAttr {
            ino: Self::to_kernel(ino),
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind,
            perm: (node.mode & 0o7777) as u16,
            nlink: node.nlink,
            uid: node.uid,
            gid: node.gid,
            rdev: node.device().unwrap_or(0) as u32,
            blksize: self.reader.superblock().block_size,
            flags: 0,
        }
    }
}

impl Filesystem for Mount {
    fn init(&mut self, _req: &Request, config: &mut fuser::KernelConfig) -> std::io::Result<()> {
        // Lookups in one directory may run at once, and symbolic link targets
        // are cached by the kernel, since an image never changes. Either is
        // simply left off by a kernel that lacks it.
        for flag in [
            fuser::InitFlags::FUSE_PARALLEL_DIROPS,
            fuser::InitFlags::FUSE_CACHE_SYMLINKS,
        ] {
            let _ = config.add_capabilities(flag);
        }
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent) = Self::to_image(parent) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self
            .reader
            .lookup(parent, name.as_bytes())
            .and_then(|ino| self.reader.inode(ino).map(|n| (ino, n)))
        {
            Some((ino, node)) => {
                if let Some(warm) = self.warm {
                    warm.looked_up(ino);
                }
                reply.entry(&FOREVER, &self.attr(ino, &node), Generation(0));
            }
            None => {
                // A reply naming inode zero tells the kernel the name does not
                // exist and lets it remember that, which saves the many
                // repeated probes a module search makes.
                let root = self.reader.root();
                match self.reader.inode(root) {
                    Some(node) => {
                        let mut absent = self.attr(root, &node);
                        absent.ino = INodeNo(0);
                        reply.entry(&FOREVER, &absent, Generation(0));
                    }
                    None => reply.error(Errno::ENOENT),
                }
            }
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match Self::to_image(ino).and_then(|i| self.reader.inode(i).map(|n| (i, n))) {
            Some((i, node)) => reply.attr(&FOREVER, &self.attr(i, &node)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match Self::to_image(ino).and_then(|i| self.reader.symlink_target(i)) {
            Some(target) => reply.data(target),
            None => reply.error(Errno::EINVAL),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        const ACCESS: i32 = 3;
        if flags.0 & ACCESS != 0 {
            reply.error(Errno::EROFS);
            return;
        }
        match Self::to_image(ino).and_then(|i| self.reader.inode(i)) {
            Some(node) if node.file_type() == Some(OurType::Directory) => {
                reply.error(Errno::EISDIR)
            }
            // An image never changes while mounted, so the kernel may keep
            // its page cache for the whole life of the mount.
            Some(_) => {
                reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE);
                if let (Some(warm), Some(i)) = (self.warm, Self::to_image(ino)) {
                    warm.opened(i);
                }
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let Some(ino) = Self::to_image(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let mut buf = vec![0u8; size as usize];
        match self.reader.read(ino, offset, &mut buf) {
            Ok(n) => {
                reply.data(&buf[..n]);
                if let Some(warm) = self.warm {
                    warm.after_read(ino, offset, n as u64);
                }
            }
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(image_ino) = Self::to_image(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(dir) = self.reader.inode(image_ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if dir.file_type() != Some(OurType::Directory) {
            reply.error(Errno::EINVAL);
            return;
        }

        let mut index = offset;
        loop {
            let full = match index {
                0 => reply.add(ino, 1, FileType::Directory, "."),
                1 => reply.add(ino, 2, FileType::Directory, ".."),
                n => {
                    let at = n - 2;
                    if at >= dir.size {
                        break;
                    }
                    let Some(entry) = self.reader.dirent(dir.payload as u32 + at as u32) else {
                        reply.error(Errno::EIO);
                        return;
                    };
                    let (Some(name), Some(node)) =
                        (self.reader.name(entry), self.reader.inode(entry.inode))
                    else {
                        reply.error(Errno::EIO);
                        return;
                    };
                    let kind = Self::kind_of(&node);
                    reply.add(
                        Self::to_kernel(entry.inode),
                        n + 1,
                        kind,
                        OsStr::from_bytes(name),
                    )
                }
            };
            if full {
                break;
            }
            index += 1;
        }
        reply.ok();
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        reply.error(Errno::EROFS);
    }

    fn mkdir(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }

    fn unlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }

    fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }

    fn create(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        reply.error(Errno::EROFS);
    }

    fn rename(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _newparent: INodeNo,
        _newname: &OsStr,
        _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        reply.error(Errno::EROFS);
    }

    fn symlink(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _link_name: &OsStr,
        _target: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }

    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        let Some(ino) = Self::to_image(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Ok(attrs) = self.reader.xattrs(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let mut names = Vec::new();
        for (name, _) in attrs {
            names.extend_from_slice(name);
            names.push(0);
        }
        Self::reply_xattr(&names, size, reply);
    }

    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        let Some(ino) = Self::to_image(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Ok(attrs) = self.reader.xattrs(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match attrs.iter().find(|(n, _)| *n == name.as_bytes()) {
            Some((_, value)) => Self::reply_xattr(value, size, reply),
            None => reply.error(Errno::ENODATA),
        }
    }

    fn setxattr(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _name: &OsStr,
        _value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        reply.error(Errno::EROFS);
    }

    fn removexattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }
}
