use pinhead::fsop::FsOperation;

// ── FsOperation::as_str() round-trip ──────────────────────────────────────

#[test]
fn test_as_str_roundtrip() {
    let cases: Vec<(FsOperation, &str)> = vec![
        (FsOperation::Lookup, "lookup"),
        (FsOperation::GetAttr, "getattr"),
        (FsOperation::SetAttr, "setattr"),
        (FsOperation::ReadDir, "readdir"),
        (FsOperation::MkDir, "mkdir"),
        (FsOperation::RmDir, "rmdir"),
        (FsOperation::OpenDir, "opendir"),
        (FsOperation::Open, "open"),
        (FsOperation::Create, "create"),
        (FsOperation::Read, "read"),
        (FsOperation::Write, "write"),
        (FsOperation::Release, "release"),
        (FsOperation::Rename, "rename"),
        (FsOperation::Unlink, "unlink"),
    ];
    for (op, expected) in &cases {
        assert_eq!(op.as_str(), *expected, "{op:?}");
    }
}
