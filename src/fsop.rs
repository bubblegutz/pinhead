/// Standard FUSE filesystem operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsOperation {
    Lookup,
    GetAttr,
    SetAttr,
    ReadLink,
    SymLink,
    Link,
    Access,
    ReadDir,
    MkDir,
    RmDir,
    OpenDir,
    Open,
    Create,
    Read,
    Write,
    Flush,
    Release,
    ReleaseDir,
    FSync,
    FSyncDir,
    StatFs,
    Rename,
    Unlink,
    MkNod,
    Forget,
}

impl FsOperation {
    /// Categorise the operation as read, write or metadata.
    pub fn category(&self) -> OpCategory {
        use FsOperation::*;
        match self {
            Write | Create | MkDir | RmDir | Unlink | Rename | SymLink | Link | MkNod | Flush
            | Release | ReleaseDir | FSync | FSyncDir | SetAttr => OpCategory::Write,

            Read | ReadDir | ReadLink | Open | OpenDir | Lookup | GetAttr | Access | StatFs => {
                OpCategory::Read
            }

            Forget => OpCategory::Metadata,
        }
    }

    /// Human-readable name.
    pub fn as_str(&self) -> &'static str {
        use FsOperation::*;
        match self {
            Lookup => "lookup",
            GetAttr => "getattr",
            SetAttr => "setattr",
            ReadLink => "readlink",
            SymLink => "symlink",
            Link => "link",
            Access => "access",
            ReadDir => "readdir",
            MkDir => "mkdir",
            RmDir => "rmdir",
            OpenDir => "opendir",
            Open => "open",
            Create => "create",
            Read => "read",
            Write => "write",
            Flush => "flush",
            Release => "release",
            ReleaseDir => "releasedir",
            FSync => "fsync",
            FSyncDir => "fsyncdir",
            StatFs => "statfs",
            Rename => "rename",
            Unlink => "unlink",
            MkNod => "mknod",
            Forget => "forget",
        }
    }
}

/// Whether an operation reads, writes, or is metadata-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCategory {
    Read,
    Write,
    Metadata,
}

/// 9P2000 protocol operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NinepOp {
    Version,
    Auth,
    Attach,
    Walk,
    Open,
    Create,
    Read,
    Write,
    Clunk,
    Remove,
    Stat,
    WStat,
    Flush,
}

impl NinepOp {
    /// Map a 9P operation to the equivalent FUSE operation.
    ///
    /// FUSE semantics take precedence — every 9P op is translated into the
    /// closest FUSE equivalent so the handler layer sees a unified operation set.
    pub fn to_fuse(self) -> FsOperation {
        use FsOperation::*;
        match self {
            NinepOp::Version => StatFs, // protocol handshake → statfs
            NinepOp::Auth => Lookup,
            NinepOp::Attach => Lookup,
            NinepOp::Walk => Lookup,  // walk each path component via lookup
            NinepOp::Open => Open,
            NinepOp::Create => Create,
            NinepOp::Read => Read,
            NinepOp::Write => Write,
            NinepOp::Clunk => Release,
            NinepOp::Remove => Unlink,
            NinepOp::Stat => GetAttr,
            NinepOp::WStat => SetAttr,
            NinepOp::Flush => Flush,
        }
    }

    /// Human-readable name.
    pub fn as_str(&self) -> &'static str {
        use NinepOp::*;
        match self {
            Version => "version",
            Auth => "auth",
            Attach => "attach",
            Walk => "walk",
            Open => "open",
            Create => "create",
            Read => "read",
            Write => "write",
            Clunk => "clunk",
            Remove => "remove",
            Stat => "stat",
            WStat => "wstat",
            Flush => "flush",
        }
    }
}
