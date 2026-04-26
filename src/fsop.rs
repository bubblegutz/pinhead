/// Standard FUSE filesystem operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsOperation {
    Lookup,
    GetAttr,
    SetAttr,
    ReadDir,
    MkDir,
    RmDir,
    OpenDir,
    Open,
    Create,
    Read,
    Write,
    Release,
    Rename,
    Unlink,
}

impl FsOperation {
    /// Human-readable name.
    pub fn as_str(&self) -> &'static str {
        use FsOperation::*;
        match self {
            Lookup => "lookup",
            GetAttr => "getattr",
            SetAttr => "setattr",

            ReadDir => "readdir",
            MkDir => "mkdir",
            RmDir => "rmdir",
            OpenDir => "opendir",
            Open => "open",
            Create => "create",
            Read => "read",
            Write => "write",
            Release => "release",
            Rename => "rename",
            Unlink => "unlink",

        }
    }
}


