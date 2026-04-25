use pinhead::fsop::{FsOperation, NinepOp, OpCategory};

// ── FsOperation::category() ────────────────────────────────────────────────

#[test]
fn test_category_read_ops() {
    for op in &[
        FsOperation::Read,
        FsOperation::ReadDir,
        FsOperation::ReadLink,
        FsOperation::Open,
        FsOperation::OpenDir,
        FsOperation::Lookup,
        FsOperation::GetAttr,
        FsOperation::Access,
        FsOperation::StatFs,
    ] {
        assert_eq!(op.category(), OpCategory::Read, "{op:?} should be Read");
    }
}

#[test]
fn test_category_write_ops() {
    for op in &[
        FsOperation::Write,
        FsOperation::Create,
        FsOperation::MkDir,
        FsOperation::RmDir,
        FsOperation::Unlink,
        FsOperation::Rename,
        FsOperation::SymLink,
        FsOperation::Link,
        FsOperation::MkNod,
        FsOperation::Flush,
        FsOperation::Release,
        FsOperation::ReleaseDir,
        FsOperation::FSync,
        FsOperation::FSyncDir,
        FsOperation::SetAttr,
    ] {
        assert_eq!(op.category(), OpCategory::Write, "{op:?} should be Write");
    }
}

#[test]
fn test_category_forget_is_metadata() {
    assert_eq!(FsOperation::Forget.category(), OpCategory::Metadata);
}

// ── FsOperation::as_str() round-trip ──────────────────────────────────────

#[test]
fn test_as_str_roundtrip() {
    // Every variant maps to a unique lowercase name.
    let cases: Vec<(FsOperation, &str)> = vec![
        (FsOperation::Lookup, "lookup"),
        (FsOperation::GetAttr, "getattr"),
        (FsOperation::SetAttr, "setattr"),
        (FsOperation::ReadLink, "readlink"),
        (FsOperation::SymLink, "symlink"),
        (FsOperation::Link, "link"),
        (FsOperation::Access, "access"),
        (FsOperation::ReadDir, "readdir"),
        (FsOperation::MkDir, "mkdir"),
        (FsOperation::RmDir, "rmdir"),
        (FsOperation::OpenDir, "opendir"),
        (FsOperation::Open, "open"),
        (FsOperation::Create, "create"),
        (FsOperation::Read, "read"),
        (FsOperation::Write, "write"),
        (FsOperation::Flush, "flush"),
        (FsOperation::Release, "release"),
        (FsOperation::ReleaseDir, "releasedir"),
        (FsOperation::FSync, "fsync"),
        (FsOperation::FSyncDir, "fsyncdir"),
        (FsOperation::StatFs, "statfs"),
        (FsOperation::Rename, "rename"),
        (FsOperation::Unlink, "unlink"),
        (FsOperation::MkNod, "mknod"),
        (FsOperation::Forget, "forget"),
    ];
    for (op, expected) in &cases {
        assert_eq!(op.as_str(), *expected, "{op:?}");
    }
}

// ── NinepOp::to_fuse() ────────────────────────────────────────────────────

#[test]
fn test_ninep_to_fuse_mapping() {
    let cases: Vec<(NinepOp, FsOperation)> = vec![
        (NinepOp::Version, FsOperation::StatFs),
        (NinepOp::Auth, FsOperation::Lookup),
        (NinepOp::Attach, FsOperation::Lookup),
        (NinepOp::Walk, FsOperation::Lookup),
        (NinepOp::Open, FsOperation::Open),
        (NinepOp::Create, FsOperation::Create),
        (NinepOp::Read, FsOperation::Read),
        (NinepOp::Write, FsOperation::Write),
        (NinepOp::Clunk, FsOperation::Release),
        (NinepOp::Remove, FsOperation::Unlink),
        (NinepOp::Stat, FsOperation::GetAttr),
        (NinepOp::WStat, FsOperation::SetAttr),
        (NinepOp::Flush, FsOperation::Flush),
    ];
    for (ninep, expected) in &cases {
        assert_eq!(ninep.to_fuse(), *expected, "{ninep:?} -> fuse");
    }
}

// ── NinepOp::as_str() round-trip ──────────────────────────────────────────

#[test]
fn test_ninep_as_str() {
    let cases: Vec<(NinepOp, &str)> = vec![
        (NinepOp::Version, "version"),
        (NinepOp::Auth, "auth"),
        (NinepOp::Attach, "attach"),
        (NinepOp::Walk, "walk"),
        (NinepOp::Open, "open"),
        (NinepOp::Create, "create"),
        (NinepOp::Read, "read"),
        (NinepOp::Write, "write"),
        (NinepOp::Clunk, "clunk"),
        (NinepOp::Remove, "remove"),
        (NinepOp::Stat, "stat"),
        (NinepOp::WStat, "wstat"),
        (NinepOp::Flush, "flush"),
    ];
    for (op, expected) in &cases {
        assert_eq!(op.as_str(), *expected, "{op:?}");
    }
}
