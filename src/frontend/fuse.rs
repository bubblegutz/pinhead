//! FUSE frontend — mounts a virtual filesystem via libfuse3.
//!
//! Implements `fuser::Filesystem` by dispatching every operation through
//! the shared path router, exactly like the 9P and SSH/SFTP frontends.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use fuser::{
    AccessFlags, BackgroundSession, BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType,
    Filesystem, FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request as FuseRequest, Session, SessionACL, TimeOrNow, WriteFlags,
};
use libc;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

use crate::fsop::FsOperation;
use crate::router::Request;

const TTL: Duration = Duration::from_secs(0);

/// FUSE filesystem that dispatches every call through the router.
pub struct FuseFilesystem {
    tx: mpsc::Sender<Request>,
    ino_next: AtomicU64,
    /// Inode → absolute path mapping (populated by `lookup`).
    paths: Mutex<HashMap<u64, String>>,
    /// Path → content length cache (populated by write/read, consumed by
    /// getattr fallback).  When a handler returns empty data for getattr
    /// (e.g. the readdir handler matched a file path), we check this cache
    /// before falling back to a full Read request.
    size_cache: Mutex<HashMap<String, u64>>,
    /// Pre-computed root attr (inode 1).
    root_attr: FileAttr,
    /// Captured tokio runtime handle for block_on from FUSE background thread.
    runtime: Handle,
}

impl FuseFilesystem {
    pub fn new(tx: mpsc::Sender<Request>) -> Self {
        let root_attr = {
            let now = SystemTime::now();
            FileAttr {
                ino: INodeNo(1),
                size: 0,
                blocks: 0,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 512,
                flags: 0,
            }
        };

        Self {
            tx,
            ino_next: AtomicU64::new(2),
            paths: Mutex::new(HashMap::from([(1, "/".to_string())])),
            size_cache: Mutex::new(HashMap::new()),
            root_attr,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    fn next_ino(&self) -> u64 {
        self.ino_next.fetch_add(1, Ordering::SeqCst)
    }

    fn record_path(&self, ino: u64, path: String) {
        self.paths.lock().unwrap().insert(ino, path);
    }

    fn path_for(&self, ino: u64) -> Option<String> {
        self.paths.lock().unwrap().get(&ino).cloned()
    }

    fn file_attr(&self, ino: u64, size: u64, is_dir: bool) -> FileAttr {
        let now = SystemTime::now();
        let kind = if is_dir {
            FileType::Directory
        } else {
            FileType::RegularFile
        };
        let perm = if is_dir { 0o755 } else { 0o644 };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: (size + 511) / 512,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind,
            perm,
            nlink: if is_dir { 2 } else { 1 },
            uid: self.root_attr.uid,
            gid: self.root_attr.gid,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    fn send_req(&self, op: FsOperation, path: &str, data: Bytes) -> Result<Bytes, String> {
        eprintln!("fuse send_req op={} path={}", op.as_str(), path);
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op,
            path: path.to_string(),
            data,
            reply: reply_tx,
        };
        self.tx
            .blocking_send(req)
            .map_err(|_| "router channel closed".to_string())?;

        // Block on the response – this runs inside the FUSE background thread,
        // so use the captured runtime handle.
        self.runtime
            .block_on(reply_rx)
            .map_err(|_| "handler did not reply".to_string())?
            .map(|r| r.data)
    }
}

unsafe impl Send for FuseFilesystem {}
unsafe impl Sync for FuseFilesystem {}

impl Filesystem for FuseFilesystem {
    fn lookup(&self, _req: &FuseRequest, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_string_lossy().to_string();
        if name == "." || name == ".." {
            let attr = if parent == INodeNo(1) {
                self.root_attr.clone()
            } else {
                let _p = self.path_for(parent.0).unwrap_or_default();
                self.file_attr(parent.0, 0, true)
            };
            return reply.entry(&TTL, &attr, Generation(0));
        }

        // Reconstruct the full path.
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let path = if parent_path.ends_with('/') {
            format!("{parent_path}{name}")
        } else {
            format!("{parent_path}/{name}")
        };

        match self.send_req(FsOperation::Lookup, &path, Bytes::new()) {
            Ok(data) => {
                let ino = self.next_ino();
                self.record_path(ino, path.clone());
                // Try getattr for accurate size; fall back to content length.
                let (parsed_is_dir, size) = match self.send_req(FsOperation::GetAttr, &path, Bytes::new()) {
                    Ok(attr_data) => {
                        let (d, s) = parse_attr(&attr_data);
                        if s == 0 && !attr_data.is_empty() && parse_attr_size(&attr_data).is_none() {
                            (false, attr_data.len() as u64)
                        } else {
                            (d, s)
                        }
                    }
                    Err(_) => (false, data.len() as u64),
                };
                reply.entry(&TTL, &self.file_attr(ino, size, parsed_is_dir), Generation(0));
            }
            Err(msg) => {
                if msg.starts_with("no route matches") {
                    // Path not found in route table. This may be an intermediate
                    // directory in a FUSE path walk (e.g. /users when the route
                    // is /users/{id}/profile). Record it as a directory so the
                    // kernel can continue walking to the final component.
                    let ino = self.next_ino();
                    self.record_path(ino, path);
                    reply.entry(&TTL, &self.file_attr(ino, 4096, true), Generation(0));
                } else {
                    // Handler returned an error (e.g. ENOENT from Lua error()).
                    reply.error(Errno::ENOENT);
                }
            }
        }
    }

    fn getattr(&self, _req: &FuseRequest, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if ino == INodeNo(1) {
            return reply.attr(&TTL, &self.root_attr);
        }

        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        match self.send_req(FsOperation::GetAttr, &path, Bytes::new()) {
            Ok(data) => {
                let (is_dir, size) = parse_attr(&data);
                // getattr returned content (no size= token) — use length
                if size == 0 && !data.is_empty() && parse_attr_size(&data).is_none() {
                    reply.attr(&TTL, &self.file_attr(ino.0, data.len() as u64, false));
                } else if size == 0 && data.is_empty() && !is_dir {
                    // Handler returned empty data (e.g. readdir handler for
                    // a file path). Check the size cache, then fall back to
                    // read to get actual content.
                    let cached = self.size_cache.lock().unwrap().get(&path).copied();
                    match cached.or_else(|| {
                        self.send_req(FsOperation::Read, &path, Bytes::new())
                            .ok()
                            .map(|d| d.len() as u64)
                    }) {
                        Some(s) => reply.attr(&TTL, &self.file_attr(ino.0, s, false)),
                        None => reply.attr(&TTL, &self.file_attr(ino.0, 0, false)),
                    }
                } else {
                    reply.attr(&TTL, &self.file_attr(ino.0, size, is_dir));
                }
            }
            Err(msg) => {
                // Fall back to lookup to verify the path still exists.
                match self.send_req(FsOperation::Lookup, &path, Bytes::new()) {
                    Ok(data) => {
                        reply.attr(&TTL, &self.file_attr(ino.0, data.len() as u64, false))
                    }
                    Err(lookup_msg) => {
                        if msg.starts_with("no route matches") && lookup_msg.starts_with("no route matches") {
                            // Path was recorded by lookup as an intermediate
                            // directory (the route table has no handler for this
                            // exact path, but it's a prefix of a real route).
                            // Return a directory attr so the kernel can continue
                            // walking to the final component.
                            reply.attr(&TTL, &self.file_attr(ino.0, 4096, true));
                        } else {
                            // Handler returned a real error (e.g. ENOENT).
                            reply.error(Errno::ENOENT);
                        }
                    }
                }
            }
        }
    }

    fn readdir(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let path = if ino == INodeNo(1) {
            "/".to_string()
        } else {
            match self.path_for(ino.0) {
                Some(p) => p,
                None => return reply.error(Errno::ENOENT),
            }
        };

        // Always include . and ..
        let entries = {
            let mut v: Vec<(INodeNo, FileType, String)> = Vec::new();
            v.push((INodeNo(1), FileType::Directory, ".".to_string()));
            if ino == INodeNo(1) {
                v.push((INodeNo(1), FileType::Directory, "..".to_string()));
            } else {
                // parent inode lookup — naively strip last component.
                let parent_path = path
                    .rsplit_once('/')
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_else(|| "/".to_string());
                let parent_ino = self
                    .paths
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(_, p)| **p == parent_path)
                    .map(|(ino, _)| INodeNo(*ino))
                    .unwrap_or(INodeNo(1));
                v.push((parent_ino, FileType::Directory, "..".to_string()));
            }

            // Ask the Lua handler for directory entries.
            let listing = self.send_req(FsOperation::ReadDir, &path, Bytes::new());
            if let Ok(data) = listing {
                let text = String::from_utf8_lossy(&data);
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line == "." || line == ".." {
                        continue;
                    }
                    let name = if let Some((key, _)) = line.split_once(':') {
                        key.trim().to_string()
                    } else {
                        line.to_string()
                    };
                    if !name.is_empty() {
                        let kind = if name.ends_with('/') { FileType::Directory } else { FileType::RegularFile };
                        let clean = name.trim_end_matches('/');
                        let e_ino = self.next_ino();
                        self.record_path(e_ino, format!("{}/{}", path.trim_end_matches('/'), clean));
                        v.push((INodeNo(e_ino), kind, clean.to_string()));
                    }
                }
            }
            v
        };

        // Track offset as u64 sequence
        let mut seq = 0u64;
        for (e_ino, kind, name) in &entries {
            seq += 1;
            if seq <= offset {
                continue;
            }
            if reply.add(*e_ino, seq, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &FuseRequest, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        match self.send_req(FsOperation::Read, &path, Bytes::new()) {
            Ok(data) => {
                self.size_cache.lock().unwrap().insert(path.clone(), data.len() as u64);
                let start = offset as usize;
                let end = (start + size as usize).min(data.len());
                let chunk = if start < data.len() {
                    data.slice(start..end)
                } else {
                    Bytes::new()
                };
                reply.data(&chunk);
            }
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn write(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        match self.send_req(
            FsOperation::Write,
            &path,
            Bytes::copy_from_slice(data),
        ) {
            Ok(resp) => {
                self.size_cache.lock().unwrap().insert(path.clone(), data.len() as u64);
                eprintln!("[fuse write] path={path} ok resp.len={}", resp.len());
                reply.written(data.len() as u32);
            }
            Err(e) => {
                eprintln!("[fuse write] path={path} error: {e}");
                reply.error(Errno::EIO);
            }
        }
    }

    fn release(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let _ = self.send_req(FsOperation::Release, &path, Bytes::new());
        reply.ok();
    }

    fn opendir(&self, _req: &FuseRequest, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        match self.send_req(FsOperation::OpenDir, &path, Bytes::new()) {
            Ok(_) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn releasedir(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let _ = self.send_req(FsOperation::Release, &path, Bytes::new());
        reply.ok();
    }

    fn forget(&self, _req: &FuseRequest, ino: INodeNo, _nlookup: u64) {
        if ino != INodeNo(1) {
            self.paths.lock().unwrap().remove(&ino.0);
        }
    }

    fn statfs(&self, _req: &FuseRequest, _ino: INodeNo, reply: ReplyStatfs) {
        reply.statfs(4096, 0, 0, 0, 0, 512, 255, 0);
    }

    fn access(&self, _req: &FuseRequest, _ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        reply.ok();
    }

    fn create(
        &self,
        _req: &FuseRequest,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let name = name.to_string_lossy().to_string();
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let path = format!("{}/{}", parent_path.trim_end_matches('/'), name);

        let ino = self.next_ino();
        self.record_path(ino, path);
        reply.created(&TTL, &self.file_attr(ino, 0, false), Generation(0), FileHandle(0), FopenFlags::empty());
    }

    fn mkdir(
        &self,
        _req: &FuseRequest,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name = name.to_string_lossy().to_string();
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let path = format!("{}/{}", parent_path.trim_end_matches('/'), name);

        match self.send_req(FsOperation::MkDir, &path, Bytes::new()) {
            Ok(_) => {
                let (is_dir, size) = match self.send_req(FsOperation::GetAttr, &path, Bytes::new()) {
                    Ok(data) => parse_attr(&data),
                    Err(_) => (true, 4096),
                };
                let ino = self.next_ino();
                self.record_path(ino, path);
                reply.entry(&TTL, &self.file_attr(ino, size, is_dir), Generation(0));
            }
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn unlink(&self, _req: &FuseRequest, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let path = format!("{}/{}", parent_path.trim_end_matches('/'), name);

        match self.send_req(FsOperation::Unlink, &path, Bytes::new()) {
            Ok(_) => reply.ok(),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn rmdir(&self, _req: &FuseRequest, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let path = format!("{}/{}", parent_path.trim_end_matches('/'), name);

        match self.send_req(FsOperation::RmDir, &path, Bytes::new()) {
            Ok(_) => reply.ok(),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn rename(
        &self,
        _req: &FuseRequest,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let name = name.to_string_lossy().to_string();
        let parent_path = self.path_for(parent.0).unwrap_or_else(|| "/".into());
        let old_path = format!("{}/{}", parent_path.trim_end_matches('/'), name);

        let newname = newname.to_string_lossy().to_string();
        let newparent_path = self.path_for(newparent.0).unwrap_or_else(|| "/".into());
        let new_path = format!("{}/{}", newparent_path.trim_end_matches('/'), newname);

        match self.send_req(FsOperation::Rename, &old_path, Bytes::from(new_path.clone())) {
            Ok(_) => {
                // Update path cache for the renamed inode.
                let ino = self.paths.lock().unwrap().iter().find_map(|(ino, p)| {
                    if *p == old_path { Some(*ino) } else { None }
                });
                if let Some(ino) = ino {
                    self.record_path(ino, new_path);
                }
                reply.ok();
            }
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn setattr(
        &self,
        _req: &FuseRequest,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtmtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let path = match self.path_for(ino.0) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        // Encode attributes as a semicolon-separated string for the Lua handler.
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = mode {
            parts.push(format!("mode={m}"));
        }
        if let Some(u) = uid {
            parts.push(format!("uid={u}"));
        }
        if let Some(g) = gid {
            parts.push(format!("gid={g}"));
        }
        if let Some(s) = size {
            parts.push(format!("size={s}"));
        }
        if let Some(TimeOrNow::SpecificTime(ts)) = atime {
            let secs = ts.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs()).unwrap_or(0);
            parts.push(format!("atime={secs}"));
        }
        if let Some(TimeOrNow::SpecificTime(ts)) = mtime {
            let secs = ts.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs()).unwrap_or(0);
            parts.push(format!("mtime={secs}"));
        }
        let payload = Bytes::from(parts.join(";"));

        match self.send_req(FsOperation::SetAttr, &path, payload) {
            Ok(data) => {
                let (is_dir, cur_size) = parse_attr(&data);
                let final_size = size.unwrap_or(cur_size);
                reply.attr(&TTL, &self.file_attr(ino.0, final_size, is_dir));
            }
            Err(_) => {
                // Fall back to getattr for current attrs.
                match self.send_req(FsOperation::GetAttr, &path, Bytes::new()) {
                    Ok(data) => {
                        let (is_dir, cur_size) = parse_attr(&data);
                        reply.attr(&TTL, &self.file_attr(ino.0, cur_size, is_dir));
                    }
                    Err(_) => reply.error(Errno::ENOENT),
                }
            }
        }
    }

    fn flush(
        &self,
        _req: &FuseRequest,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &FuseRequest,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsyncdir(
        &self,
        _req: &FuseRequest,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }
}

// ── mount ──────────────────────────────────────────────────────────────────

/// Mount the FUSE filesystem at `mountpoint` and return a `BackgroundSession`.
///
/// The session runs in a background thread. When the `BackgroundSession` is
/// dropped, the filesystem is automatically unmounted.
pub fn mount(tx: mpsc::Sender<Request>, mountpoint: &str) -> Result<BackgroundSession, String> {
    let fs = FuseFilesystem::new(tx);
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("pinhead".to_string()),
        MountOption::NoAtime,
    ];
    config.acl = SessionACL::All;
    let session = Session::new(fs, mountpoint, &config)
        .map_err(|e| format!("FUSE mount: {e}"))?;

    session.spawn().map_err(|e| format!("FUSE spawn: {e}"))
}

// ── Attribute parsing helpers ────────────────────────────────────────────────

/// Parse a `"mode=TYPE size=N"` string from a Lua getattr handler.
/// Returns `(is_dir, size)`.
fn parse_attr(data: &[u8]) -> (bool, u64) {
    let text = String::from_utf8_lossy(data);
    let mut is_dir = false;
    let mut size = 0u64;

    for part in text.split_whitespace() {
        if let Some(val) = part.strip_prefix("mode=") {
            is_dir = val.trim() == "dir" || val.trim() == "directory";
        } else if let Some(val) = part.strip_prefix("size=") {
            size = val.parse::<u64>().unwrap_or(0);
        }
    }
    (is_dir, size)
}

/// Parse a `"mode=TYPE size=N"` string and return just the size.
fn parse_attr_size(data: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(data);
    for part in text.split_whitespace() {
        if let Some(val) = part.strip_prefix("size=") {
            return val.parse::<u64>().ok();
        }
    }
    None
}
