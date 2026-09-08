//! Exposing an image through FUSE.
//!
//! The mount is read only. Timestamps are reported as the Unix epoch, because
//! the format records none; that is what keeps images byte-identical between
//! builds.

use crate::format::{FileType as OurType, Inode};
use crate::read::Reader;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, OpenFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, Request,
};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

/// How long the kernel may trust an attribute or lookup result.
///
/// An image never changes while mounted, so entries never need revalidating.
const FOREVER: Duration = Duration::from_secs(60 * 60 * 24 * 365);

/// A mounted image.
pub struct Mount {
    reader: &'static Reader<'static>,
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
        Ok(Mount { reader })
    }

    /// The kernel numbers the root 1, while the format numbers it 0.
    fn to_image(ino: INodeNo) -> Option<u32> {
        u32::try_from(ino.0.checked_sub(1)?).ok()
    }

    fn to_kernel(ino: u32) -> INodeNo {
        INodeNo(ino as u64 + 1)
    }

    fn attr(&self, ino: u32, node: &Inode) -> FileAttr {
        let kind = match node.file_type() {
            Some(OurType::Directory) => FileType::Directory,
            Some(OurType::Symlink) => FileType::Symlink,
            _ => FileType::RegularFile,
        };
        let size = match node.file_type() {
            Some(OurType::Directory) => 0,
            _ => node.size,
        };
        FileAttr {
            ino: Self::to_kernel(ino),
            size,
            blocks: size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm: (node.mode & 0o7777) as u16,
            nlink: node.nlink,
            uid: node.uid,
            gid: node.gid,
            rdev: 0,
            blksize: self.reader.superblock().block_size,
            flags: 0,
        }
    }

    fn find(&self, parent: u32, name: &[u8]) -> Option<u32> {
        let dir = self.reader.inode(parent)?;
        if dir.file_type() != Some(OurType::Directory) {
            return None;
        }
        let first = u32::try_from(dir.payload).ok()?;
        let mut lo = 0u32;
        let mut hi = u32::try_from(dir.size).ok()?;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = self.reader.dirent(first + mid)?;
            match self.reader.name(entry)?.cmp(name) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(entry.inode),
            }
        }
        None
    }
}

impl Filesystem for Mount {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent) = Self::to_image(parent) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self
            .find(parent, name.as_bytes())
            .and_then(|ino| self.reader.inode(ino).map(|n| (ino, n)))
        {
            Some((ino, node)) => reply.entry(&FOREVER, &self.attr(ino, &node), Generation(0)),
            None => reply.error(Errno::ENOENT),
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
            Some(_) => reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE),
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
            Ok(n) => reply.data(&buf[..n]),
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
                    let kind = match node.file_type() {
                        Some(OurType::Directory) => FileType::Directory,
                        Some(OurType::Symlink) => FileType::Symlink,
                        _ => FileType::RegularFile,
                    };
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
}
