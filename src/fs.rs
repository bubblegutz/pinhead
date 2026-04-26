use std::fs;
use std::path::Path;

use rlua::Lua;

/// Register `fs.*` Lua API functions for real filesystem access.
///
/// # Read operations
///   fs.read(path)         -> string | nil
///   fs.readdir(path)      -> table of {name, is_dir, is_file, size} | nil
///   fs.stat(path)         -> {size, is_dir, is_file, is_symlink, mode, uid, gid, mtime} | nil
///   fs.exists(path)       -> boolean
///   fs.is_dir(path)       -> boolean
///   fs.is_file(path)      -> boolean
///   fs.ls(path)           -> {name, ...} | nil
///
/// # Write / create operations
///   fs.write(path, content)     -> true | nil
///   fs.remove(path)             -> true | nil   (file or empty dir)
///   fs.remove_all(path)         -> true | nil   (recursive removal)
///   fs.rename(old, new)         -> true | nil
///   fs.copy(src, dst)           -> true | nil
///   fs.mkdir(path)              -> true | nil   (single dir)
///   fs.mkdir_all(path)          -> true | nil   (recursive mkdir)
///
/// # Attribute operations (Unix-only; return nil on other platforms)
///   fs.chmod(path, mode)        -> true | nil   (mode is numeric e.g. 0o755)
///   fs.chown(path, uid, gid)    -> true | nil
///   fs.utimens(path, atime, mtime) -> true | nil  (seconds since epoch)
///
/// # Utility
///   fs.mode_string(mode)        -> string  (e.g. "rwxr-xr-x")
pub fn register_lua_apis(lua: &Lua) -> Result<(), String> {
    let t = lua.create_table().map_err(|e| format!("{e}"))?;

    // --- Read operations ---

    // fs.read(path) -> string | nil
    {
        let f = lua
            .create_function(|_, path: String| -> Result<Option<String>, rlua::Error> {
                match fs::read_to_string(&path) {
                    Ok(s) => Ok(Some(s)),
                    Err(_) => Ok(None),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("read", f).map_err(|e| format!("{e}"))?;
    }

    // fs.readdir(path) -> table of entry tables | nil
    {
        let f = lua
            .create_function(|lua, path: String| -> Result<Option<rlua::Table>, rlua::Error> {
                let entries = match fs::read_dir(&path) {
                    Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
                    Err(_) => return Ok(None),
                };
                let t = lua.create_table()?;
                for (i, entry) in entries.iter().enumerate() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let e = lua.create_table()?;
                    e.set("name", name)?;
                    let meta = entry.metadata();
                    if let Ok(m) = meta {
                        e.set("is_dir", m.is_dir())?;
                        e.set("is_file", m.is_file())?;
                        e.set("size", m.len() as i64)?;
                    }
                    t.set(i + 1, e)?;
                }
                Ok(Some(t))
            })
            .map_err(|e| format!("{e}"))?;
        t.set("readdir", f).map_err(|e| format!("{e}"))?;
    }

    // fs.stat(path) -> table | nil
    {
        let f = lua
            .create_function(|lua, path: String| -> Result<Option<rlua::Table>, rlua::Error> {
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => return Ok(None),
                };
                let t = lua.create_table()?;
                t.set("size", meta.len() as i64)?;
                t.set("is_dir", meta.is_dir())?;
                t.set("is_file", meta.is_file())?;
                t.set("is_symlink", meta.file_type().is_symlink())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    t.set("mode", meta.mode())?;
                    t.set("uid", meta.uid())?;
                    t.set("gid", meta.gid())?;
                }
                #[cfg(not(unix))]
                {
                    t.set("mode", 0i64)?;
                }
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                t.set("mtime", mtime)?;
                Ok(Some(t))
            })
            .map_err(|e| format!("{e}"))?;
        t.set("stat", f).map_err(|e| format!("{e}"))?;
    }

    // fs.exists(path) -> boolean
    {
        let f = lua
            .create_function(|_, path: String| -> Result<bool, rlua::Error> {
                Ok(Path::new(&path).exists())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("exists", f).map_err(|e| format!("{e}"))?;
    }

    // fs.is_dir(path) -> boolean
    {
        let f = lua
            .create_function(|_, path: String| -> Result<bool, rlua::Error> {
                Ok(Path::new(&path).is_dir())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("is_dir", f).map_err(|e| format!("{e}"))?;
    }

    // fs.is_file(path) -> boolean
    {
        let f = lua
            .create_function(|_, path: String| -> Result<bool, rlua::Error> {
                Ok(Path::new(&path).is_file())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("is_file", f).map_err(|e| format!("{e}"))?;
    }

    // fs.ls(path) -> {name, ...} | nil
    {
        let f = lua
            .create_function(|lua, path: String| -> Result<Option<rlua::Table>, rlua::Error> {
                let entries = match fs::read_dir(&path) {
                    Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
                    Err(_) => return Ok(None),
                };
                let t = lua.create_table()?;
                for (i, entry) in entries.iter().enumerate() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    t.set(i + 1, name)?;
                }
                Ok(Some(t))
            })
            .map_err(|e| format!("{e}"))?;
        t.set("ls", f).map_err(|e| format!("{e}"))?;
    }

    // --- Write / create operations ---

    // fs.write(path, content) -> true | nil
    {
        let f = lua
            .create_function(
                |_, (path, content): (String, String)| -> Result<Option<bool>, rlua::Error> {
                    match fs::write(&path, &content) {
                        Ok(()) => Ok(Some(true)),
                        Err(_) => Ok(None),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("write", f).map_err(|e| format!("{e}"))?;
    }

    // fs.remove(path) -> true | nil
    // Removes files and empty directories.
    {
        let f = lua
            .create_function(|_, path: String| -> Result<Option<bool>, rlua::Error> {
                // Try as file first, then as directory.
                match fs::remove_file(&path) {
                    Ok(()) => Ok(Some(true)),
                    Err(_) => match fs::remove_dir(&path) {
                        Ok(()) => Ok(Some(true)),
                        Err(_) => Ok(None),
                    },
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("remove", f).map_err(|e| format!("{e}"))?;
    }

    // fs.remove_all(path) -> true | nil
    // Recursively removes a file or directory tree.
    {
        let f = lua
            .create_function(|_, path: String| -> Result<Option<bool>, rlua::Error> {
                match fs::remove_dir_all(&path) {
                    Ok(()) => Ok(Some(true)),
                    Err(_) => Ok(None),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("remove_all", f).map_err(|e| format!("{e}"))?;
    }

    // fs.rename(old, new) -> true | nil
    {
        let f = lua
            .create_function(
                |_, (old, new): (String, String)| -> Result<Option<bool>, rlua::Error> {
                    match fs::rename(&old, &new) {
                        Ok(()) => Ok(Some(true)),
                        Err(_) => Ok(None),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("rename", f).map_err(|e| format!("{e}"))?;
    }

    // fs.copy(src, dst) -> true | nil
    {
        let f = lua
            .create_function(
                |_, (src, dst): (String, String)| -> Result<Option<bool>, rlua::Error> {
                    match fs::copy(&src, &dst) {
                        Ok(_) => Ok(Some(true)),
                        Err(_) => Ok(None),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("copy", f).map_err(|e| format!("{e}"))?;
    }

    // fs.mkdir(path) -> true | nil
    {
        let f = lua
            .create_function(|_, path: String| -> Result<Option<bool>, rlua::Error> {
                match fs::create_dir(&path) {
                    Ok(()) => Ok(Some(true)),
                    Err(_) => Ok(None),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("mkdir", f).map_err(|e| format!("{e}"))?;
    }

    // fs.mkdir_all(path) -> true | nil
    {
        let f = lua
            .create_function(|_, path: String| -> Result<Option<bool>, rlua::Error> {
                match fs::create_dir_all(&path) {
                    Ok(()) => Ok(Some(true)),
                    Err(_) => Ok(None),
                }
            })
            .map_err(|e| format!("{e}"))?;
        t.set("mkdir_all", f).map_err(|e| format!("{e}"))?;
    }

    // --- Attribute operations ---

    // fs.chmod(path, mode) -> true | nil  (Unix only)
    #[cfg(unix)]
    {
        let f = lua
            .create_function(
                |_, (path, mode): (String, u32)| -> Result<Option<bool>, rlua::Error> {
                    use std::os::unix::fs::PermissionsExt;
                    let perm = fs::Permissions::from_mode(mode);
                    match fs::set_permissions(&path, perm) {
                        Ok(()) => Ok(Some(true)),
                        Err(_) => Ok(None),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("chmod", f).map_err(|e| format!("{e}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = t.set("chmod", ());
    }

    // fs.chown(path, uid, gid) -> true | nil  (Unix only)
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let f = lua
            .create_function(
                |_, (path, uid, gid): (String, u32, u32)| -> Result<Option<bool>, rlua::Error> {
                    let cpath = CString::new(path.as_str())
                        .map_err(|_| rlua::Error::ToLuaConversionError {
                            from: "String",
                            to: "CString",
                            message: Some("path contains null byte".into()),
                        })?;
                    let ret = unsafe { libc::chown(cpath.as_ptr(), uid, gid) };
                    if ret == 0 { Ok(Some(true)) } else { Ok(None) }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("chown", f).map_err(|e| format!("{e}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = t.set("chown", ());
    }

    // fs.utimens(path, atime, mtime) -> true | nil  (Unix only)
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let f = lua
            .create_function(
                |_, (path, atime, mtime): (String, i64, i64)| -> Result<Option<bool>, rlua::Error> {
                    let cpath = CString::new(path.as_str())
                        .map_err(|_| rlua::Error::ToLuaConversionError {
                            from: "String",
                            to: "CString",
                            message: Some("path contains null byte".into()),
                        })?;
                    let times = [
                        libc::timespec {
                            tv_sec: atime,
                            tv_nsec: 0,
                        },
                        libc::timespec {
                            tv_sec: mtime,
                            tv_nsec: 0,
                        },
                    ];
                    let ret = unsafe {
                        libc::utimensat(
                            libc::AT_FDCWD,
                            cpath.as_ptr(),
                            &times as *const libc::timespec,
                            0,
                        )
                    };
                    if ret == 0 { Ok(Some(true)) } else { Ok(None) }
                },
            )
            .map_err(|e| format!("{e}"))?;
        t.set("utimens", f).map_err(|e| format!("{e}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = t.set("utimens", ());
    }

    // --- Utility ---

    // fs.mode_string(mode) -> string
    {
        let f = lua
            .create_function(|_, mode: u32| -> Result<String, rlua::Error> {
                Ok(mode_string(mode))
            })
            .map_err(|e| format!("{e}"))?;
        t.set("mode_string", f).map_err(|e| format!("{e}"))?;
    }

    lua.globals().set("fs", t).map_err(|e| format!("{e}"))?;

    Ok(())
}

/// Convert a numeric Unix file mode to an "rwx"-style string (permission bits only).
fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    // owner
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    // group
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    // other
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    s
}
