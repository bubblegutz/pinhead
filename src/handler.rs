use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use mlua::{Lua, RegistryKey, Value};
use tokio::sync::oneshot;

use crate::serialize;
use crate::store;

// ---------------------------------------------------------------------------
// Handler request / response
// ---------------------------------------------------------------------------

/// A request sent from the router to a handler task.
pub struct HandlerRequest {
    pub params: HashMap<String, String>,
    pub data: Bytes,
    /// Name of the registered Lua function that should handle this request.
    pub handler_name: String,
    pub reply: oneshot::Sender<Result<HandlerResponse, String>>,
}

/// A response returned by a handler task.
#[derive(Debug)]
pub struct HandlerResponse {
    pub data: Bytes,
    pub matched_pattern: Option<String>,
    pub has_children: bool,
}

// ---------------------------------------------------------------------------
// Route registration (returned from Lua setup)
// ---------------------------------------------------------------------------

/// A single route registered by the Lua script.
#[derive(Debug, Clone)]
pub struct RouteRegistration {
    /// matchit path pattern (e.g. `/users/{id}/profile`).
    pub pattern: String,
    /// Name used to look up the Lua function in the handler's registry.
    pub handler_name: String,
    /// Operation names this handler is registered for.
    /// Empty = handles all operations (wildcard).
    pub ops: Vec<String>,
}

// ---------------------------------------------------------------------------
// Configuration (returned from Lua setup)
// ---------------------------------------------------------------------------

/// Configuration values set by the Lua script via `fuse.*()` / `ninep.*()` / `sshfs.*()`.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub fuse_mounts: Vec<String>,
    pub ninep_listeners: Vec<String>,
    pub sshfs_listeners: Vec<String>,
    pub sshfs_password: Option<String>,
    pub sshfs_authorized_keys_path: Option<String>,
    pub sshfs_userpasswds: Vec<(String, String)>,
}

/// Compiled bytecode for all registered Lua handler functions, plus the
/// optional default handler.  `Send + Sync` — share across workers via `Arc`.
#[derive(Debug, Clone)]
pub struct SharedBytecodes {
    /// The original Lua script text.  Re-executed in each worker to create
    /// fresh closures with correct upvalues.
    pub script: String,
    /// Working directory for `setup_runtime_apis`.
    pub cwd: std::path::PathBuf,
}

/// Worker pool configuration, set from Lua via `worker.min()`, `worker.max()`,
/// and `worker.ttl()`.
#[derive(Debug)]
pub struct WorkerConfig {
    pub min_workers: Arc<AtomicUsize>,
    pub max_workers: Arc<AtomicUsize>,
    pub ttl_secs: Arc<AtomicU64>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            min_workers: Arc::new(AtomicUsize::new(1)),
            max_workers: Arc::new(AtomicUsize::new(4)),
            ttl_secs: Arc::new(AtomicU64::new(60)),
        }
    }
}

// ---------------------------------------------------------------------------
// Lua handler runtime — !Send, must run on one thread via LocalSet
// ---------------------------------------------------------------------------

/// The Lua runtime state.  Holds the Lua VM and the registered function keys.
/// `!Send` — must be used via `spawn_local` inside a `tokio::task::LocalSet`.
#[allow(dead_code)]
pub struct HandlerRuntime {
    lua: Lua,
    funcs: HashMap<String, RegistryKey>,
    default_handler: Option<RegistryKey>,
}

#[allow(dead_code)]
impl HandlerRuntime {
    /// Compile a Lua script and produce route registrations, config, and
    /// pre-compiled handler bytecodes.  Runs synchronously — call before
    /// entering the async section of `main`.
    ///
    /// The script registers routes via `route.register(pattern, ops, func)`
    /// and `route.default(func)`, sets frontend config via `fuse.*()` /
    /// `ninep.*()` / `sshfs.*()`, and configures the worker pool via
    /// `worker.min()`, `worker.max()`, `worker.ttl()`.
    ///
    /// Returns `(Config, Vec<RouteRegistration>, SharedBytecodes, WorkerConfig)`.
    /// The bytecodes can be shared zero-copy across multiple worker `Lua` states.
    ///
    /// `cwd` — working directory for Lua's `dofile()`, `loadfile()`, and
    /// `require()`.  Defaults to the script file's parent directory, or the
    /// process CWD for piped scripts.
    pub fn compile(script: &str, cwd: &std::path::Path) -> Result<(Config, Vec<RouteRegistration>, SharedBytecodes, WorkerConfig), String> {
        let lua = Lua::new();

        // ── Shared state for route registration ──────────────────────────
        let routes = Arc::new(Mutex::new(Vec::new()));
        let funcs: Arc<Mutex<HashMap<String, RegistryKey>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let default_handler: Arc<Mutex<Option<RegistryKey>>> =
            Arc::new(Mutex::new(None));

        // ── Shared state for configuration ───────────────────────────────
        let fuse_mounts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ninep_listeners: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sshfs_listeners: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sshfs_password: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sshfs_authorized_keys_path: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sshfs_userpasswds: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        // ── Build the `route` table ──────────────────────────────────────
        {
            let route_table = lua.create_table().map_err(|e| format!("{e}"))?;

            // route.register(pattern, ops, func)
            {
                let routes = routes.clone();
                let funcs = funcs.clone();
                let register_fn = lua
                    .create_function(
                        move |lua, (pattern, ops_val, func): (String, mlua::Value, mlua::Function)| {
                            let name =
                                format!("__route_{}", routes.lock().unwrap().len());
                            let key = lua.create_registry_value(func)?;

                            // Parse ops: nil → all ops, string → single op,
                            // table → set of op strings.
                            let ops: Vec<String> = match &ops_val {
                                mlua::Value::Nil | mlua::Value::Boolean(true) => Vec::new(),
                                mlua::Value::String(s) => {
                                    vec![s.to_str()?.to_string()]
                                }
                                mlua::Value::Table(t) => {
                                    let mut v = Vec::new();
                                    for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
                                        let (_, val) = pair?;
                                        if let mlua::Value::String(s) = val {
                                            v.push(s.to_str()?.to_string());
                                        }
                                    }
                                    v
                                }
                                _ => {
                                    return Err(mlua::Error::RuntimeError(
                                        "ops must be a string, table, nil, or true".into(),
                                    ))
                                }
                            };

                            routes.lock().unwrap().push(RouteRegistration {
                                pattern,
                                handler_name: name.clone(),
                                ops,
                            });
                            funcs.lock().unwrap().insert(name, key);
                            Ok(())
                        },
                    )
                    .map_err(|e| format!("{e}"))?;
                route_table
                    .set("register", register_fn)
                    .map_err(|e| format!("{e}"))?;
            }

            // route.default(func)
            {
                let default_handler = default_handler.clone();
                let default_fn = lua
                    .create_function(move |lua, func: mlua::Function| {
                        let key = lua.create_registry_value(func)?;
                        *default_handler.lock().unwrap() = Some(key);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                route_table
                    .set("default", default_fn)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals()
                .set("route", route_table)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Route convenience wrappers ────────────────────────────────────
        // These expand route.register() into shortcut functions like route.read(), route.write(), etc.
        // Must run after the `route` table is set up but before the user script executes.
        lua.load(r#"
do
    route.read = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "release", "flush"}, func)
    end })
    route.read.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "open", "read", "release", "flush"}, func)
    end

    route.write = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "write", "release", "flush", "fsync", "create"}, func)
    end })
    route.write.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "open", "read", "write", "release", "flush", "fsync", "create"}, func)
    end

    route.readdir = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end })
    route.readdir.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end

    route.create = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "create", "open", "read", "write", "release", "flush"}, func)
    end })
    route.create.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "create", "open", "read", "write", "release", "flush"}, func)
    end

    route.unlink = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"unlink", "lookup", "getattr"}, func)
    end })
    route.unlink.default = function(func)
        route.register("/{*path}", {"unlink", "lookup", "getattr"}, func)
    end

    route.lookup = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "lookup", func)
    end })
    route.lookup.default = function(func)
        route.register("/{*path}", "lookup", func)
    end

    route.getattr = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "getattr", func)
    end })
    route.getattr.default = function(func)
        route.register("/{*path}", "getattr", func)
    end

    route.open = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "open", func)
    end })
    route.open.default = function(func)
        route.register("/{*path}", "open", func)
    end

    route.release = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "release", func)
    end })
    route.release.default = function(func)
        route.register("/{*path}", "release", func)
    end

    route.mkdir = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"mkdir", "lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end })
    route.mkdir.default = function(func)
        route.register("/{*path}", {"mkdir", "lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end

    route.all = setmetatable({}, { __call = function(_, path, func)
        route.register(path, true, func)
    end })
    route.all.default = function(func)
        route.register("/{*path}", true, func)
    end
end
"#)
            .exec()
            .map_err(|e| format!("route wrappers error: {e}"))?;

        // ── Build the `fuse` table ────────────────────────────────────────
        {
            let fuse_table = lua.create_table().map_err(|e| format!("{e}"))?;

            // fuse.mount(path)
            {
                let mounts = fuse_mounts.clone();
                let fn_ = lua
                    .create_function(move |_, path: String| {
                        mounts.lock().unwrap().push(path);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                fuse_table.set("mount", fn_).map_err(|e| format!("{e}"))?;
            }

            // fuse.unmount(path)
            {
                let mounts = fuse_mounts.clone();
                let fn_ = lua
                    .create_function(move |_, path: String| {
                        mounts.lock().unwrap().retain(|p| p != &path);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                fuse_table.set("unmount", fn_).map_err(|e| format!("{e}"))?;
            }

            // fuse.unmountall()
            {
                let mounts = fuse_mounts.clone();
                let fn_ = lua
                    .create_function(move |_, ()| {
                        mounts.lock().unwrap().clear();
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                fuse_table
                    .set("unmountall", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals()
                .set("fuse", fuse_table)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Build the `ninep` table ───────────────────────────────────────
        {
            let ninep_table = lua.create_table().map_err(|e| format!("{e}"))?;

            // ninep.listen(addr)
            {
                let listeners = ninep_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, addr: String| {
                        listeners.lock().unwrap().push(addr);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                ninep_table
                    .set("listen", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            // ninep.kill(addr)
            {
                let listeners = ninep_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, addr: String| {
                        listeners.lock().unwrap().retain(|a| a != &addr);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                ninep_table.set("kill", fn_).map_err(|e| format!("{e}"))?;
            }

            // ninep.killall()
            {
                let listeners = ninep_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, ()| {
                        listeners.lock().unwrap().clear();
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                ninep_table
                    .set("killall", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals()
                .set("ninep", ninep_table)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Build the `sshfs` table ──────────────────────────────────────
        {
            let sshfs_table = lua.create_table().map_err(|e| format!("{e}"))?;

            // sshfs.listen(addr)
            {
                let listeners = sshfs_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, addr: String| {
                        listeners.lock().unwrap().push(addr);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table
                    .set("listen", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            // sshfs.kill(addr)
            {
                let listeners = sshfs_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, addr: String| {
                        listeners.lock().unwrap().retain(|a| a != &addr);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table.set("kill", fn_).map_err(|e| format!("{e}"))?;
            }

            // sshfs.killall()
            {
                let listeners = sshfs_listeners.clone();
                let fn_ = lua
                    .create_function(move |_, ()| {
                        listeners.lock().unwrap().clear();
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table
                    .set("killall", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            // sshfs.password(pw)
            {
                let password = sshfs_password.clone();
                let fn_ = lua
                    .create_function(move |_, pw: String| {
                        *password.lock().unwrap() = Some(pw);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table
                    .set("password", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            // sshfs.authorized_keys(path)
            {
                let keys = sshfs_authorized_keys_path.clone();
                let fn_ = lua
                    .create_function(move |_, path: String| {
                        *keys.lock().unwrap() = Some(path);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table
                    .set("authorized_keys", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            // sshfs.userpasswd(username, password)
            {
                let pairs = sshfs_userpasswds.clone();
                let fn_ = lua
                    .create_function(move |_, (user, pw): (String, String)| {
                        pairs.lock().unwrap().push((user, pw));
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                sshfs_table
                    .set("userpasswd", fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals()
                .set("sshfs", sshfs_table)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Build the `worker` table (configurable min/max/ttl) ──────────
        let worker_config = WorkerConfig::default();
        {
            let t = lua.create_table().map_err(|e| format!("{e}"))?;

            // worker.min(n)
            let min = worker_config.min_workers.clone();
            let fn_ = lua
                .create_function(move |_, n: usize| {
                    min.store(n, Ordering::Release);
                    Ok(())
                })
                .map_err(|e| format!("{e}"))?;
            t.set("min", fn_).map_err(|e| format!("{e}"))?;

            // worker.max(n)
            let max = worker_config.max_workers.clone();
            let fn_ = lua
                .create_function(move |_, n: usize| {
                    max.store(n, Ordering::Release);
                    Ok(())
                })
                .map_err(|e| format!("{e}"))?;
            t.set("max", fn_).map_err(|e| format!("{e}"))?;

            // worker.ttl(seconds)
            let ttl = worker_config.ttl_secs.clone();
            let fn_ = lua
                .create_function(move |_, s: u64| {
                    ttl.store(s, Ordering::Release);
                    Ok(())
                })
                .map_err(|e| format!("{e}"))?;
            t.set("ttl", fn_).map_err(|e| format!("{e}"))?;

            lua.globals()
                .set("worker", t)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Set up runtime API tables (json.*, yaml.*, etc.) ────────────
        setup_runtime_apis(&lua, cwd)?;

        // Execute the user script.
        lua.load(script)
            .exec()
            .map_err(|e| format!("Lua script error: {e}"))?;

        // Extract route data.
        let registered = {
            let mut g = routes.lock().unwrap();
            std::mem::take(&mut *g)
        };
        let _funcs = {
            let mut g = funcs.lock().unwrap();
            std::mem::take(&mut *g)
        };
        let _default = {
            let mut g = default_handler.lock().unwrap();
            g.take()
        };

        // Extract config.
        let cfg = Config {
            fuse_mounts: fuse_mounts.lock().unwrap().clone(),
            ninep_listeners: ninep_listeners.lock().unwrap().clone(),
            sshfs_listeners: sshfs_listeners.lock().unwrap().clone(),
            sshfs_password: sshfs_password.lock().unwrap().clone(),
            sshfs_authorized_keys_path: sshfs_authorized_keys_path.lock().unwrap().clone(),
            sshfs_userpasswds: sshfs_userpasswds.lock().unwrap().clone(),
        };

        // ── Build SharedBytecodes ──────────────────────────────────────────

        Ok((
            cfg,
            registered,
            SharedBytecodes {
                script: script.to_string(),
                cwd: cwd.to_path_buf(),
            },
            worker_config,
        ))
    }

    /// Call the matching Lua function for a single request.
    pub(crate) fn call_lua(&self, req: &HandlerRequest) -> Result<HandlerResponse, String> {
        // Find the registered function.
        let key = self.funcs.get(&req.handler_name);
        let func = match key {
            Some(key) => self
                .lua
                .registry_value::<mlua::Function>(key)
                .map_err(|e| {
                    format!("failed to get Lua function `{}`: {e}", req.handler_name)
                })?,
            None => match self.default_handler.as_ref() {
                Some(key) => self
                    .lua
                    .registry_value::<mlua::Function>(key)
                    .map_err(|e| format!("failed to get default handler: {e}"))?,
                None => {
                    return Err(format!(
                        "no handler for `{}` and no default handler",
                        req.handler_name
                    ));
                }
            },
        };

        // Build params table.
        let params = self
            .lua
            .create_table()
            .map_err(|e| format!("failed to create params table: {e}"))?;
        for (k, v) in &req.params {
            params
                .set(k.as_str(), v.as_str())
                .map_err(|e| format!("failed to set param `{k}`: {e}"))?;
        }

        let data_val: Value = if req.data.is_empty() {
            Value::Nil
        } else {
            self.lua
                .create_string(&req.data)
                .map(Value::String)
                .map_err(|e| e.to_string())?
        };

        // Call the Lua function; it returns a string.
        // op is already determined by the route registration, so
        // we call with (params, data) only.
        let result: String = func
            .call::<_, String>((params, data_val))
            .map_err(|e| format!("Lua handler error: {e}"))?;

        Ok(HandlerResponse {
            data: Bytes::from(result),
            matched_pattern: None,
            has_children: false,
        })
    }

    /// Construct a HandlerRuntime from a `SharedBytecodes` (script + handler
    /// names).  Creates a fresh Lua state, sets up runtime APIs, re-executes
    /// the script to create closures with correct upvalues, then reads the
    /// handler functions from the registry.
    pub(crate) fn from_bytecodes(
        lua: Lua,
        bytecodes: &SharedBytecodes,
    ) -> Result<Self, String> {
        // Set up runtime API tables (json.*, yaml.*, etc.).
        setup_runtime_apis(&lua, &bytecodes.cwd)?;

        // Set up route.* table with local Arcs (discarded after execution).
        let routes: Arc<Mutex<Vec<RouteRegistration>>> = Arc::new(Mutex::new(Vec::new()));
        let funcs: Arc<Mutex<HashMap<String, RegistryKey>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let default_handler: Arc<Mutex<Option<RegistryKey>>> =
            Arc::new(Mutex::new(None));

        {
            let route_table = lua.create_table().map_err(|e| format!("{e}"))?;

            // route.register(pattern, ops, func)
            {
                let routes = routes.clone();
                let funcs = funcs.clone();
                let register_fn = lua
                    .create_function(
                        move |lua, (pattern, ops_val, func): (String, mlua::Value, mlua::Function)| {
                            let name =
                                format!("__route_{}", routes.lock().unwrap().len());
                            let key = lua.create_registry_value(func)?;

                            let ops: Vec<String> = match &ops_val {
                                mlua::Value::Nil | mlua::Value::Boolean(true) => Vec::new(),
                                mlua::Value::String(s) => {
                                    vec![s.to_str()?.to_string()]
                                }
                                mlua::Value::Table(t) => {
                                    let mut v = Vec::new();
                                    for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
                                        let (_, val) = pair?;
                                        if let mlua::Value::String(s) = val {
                                            v.push(s.to_str()?.to_string());
                                        }
                                    }
                                    v
                                }
                                _ => {
                                    return Err(mlua::Error::RuntimeError(
                                        "ops must be a string, table, nil, or true".into(),
                                    ))
                                }
                            };

                            routes.lock().unwrap().push(RouteRegistration {
                                pattern,
                                handler_name: name.clone(),
                                ops,
                            });
                            funcs.lock().unwrap().insert(name, key);
                            Ok(())
                        },
                    )
                    .map_err(|e| format!("{e}"))?;
                route_table
                    .set("register", register_fn)
                    .map_err(|e| format!("{e}"))?;
            }

            // route.default(func)
            {
                let default_handler = default_handler.clone();
                let default_fn = lua
                    .create_function(move |lua, func: mlua::Function| {
                        let key = lua.create_registry_value(func)?;
                        *default_handler.lock().unwrap() = Some(key);
                        Ok(())
                    })
                    .map_err(|e| format!("{e}"))?;
                route_table
                    .set("default", default_fn)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals()
                .set("route", route_table)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Stub frontend tables (fuse.*, ninep.*, sshfs.*) ─────────────
        // In the worker, frontend config has already been collected during
        // compile().  These stubs exist so the user script doesn't error
        // when it references them.
        for (name, methods) in [
            ("fuse", &["mount", "unmount", "unmountall"] as &[&str]),
            ("ninep", &["listen", "kill", "killall"]),
            ("sshfs", &["listen", "kill", "killall", "password", "authorized_keys", "userpasswd"]),
        ] {
            let table = lua.create_table().map_err(|e| format!("{e}"))?;
            for method in methods {
                let fn_ = lua
                    .create_function(move |_, ()| Ok(()))
                    .map_err(|e| format!("{e}"))?;
                table
                    .set(*method, fn_)
                    .map_err(|e| format!("{e}"))?;
            }
            lua.globals()
                .set(name, table)
                .map_err(|e| format!("{e}"))?;
        }
        // ── Stub worker.* table (no-op methods for re-execution) ──────────
        {
            let t = lua.create_table().map_err(|e| format!("{e}"))?;
            for &method in &["min", "max", "ttl"] {
                let fn_ = lua
                    .create_function(|_, ()| Ok(()))
                    .map_err(|e| format!("{e}"))?;
                t.set(method, fn_).map_err(|e| format!("{e}"))?;
            }
            lua.globals()
                .set("worker", t)
                .map_err(|e| format!("{e}"))?;
        }

        // Re-execute the convenience wrapper script too.
        lua.load(r#"
do
    route.read = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "release", "flush"}, func)
    end })
    route.read.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "open", "read", "release", "flush"}, func)
    end

    route.write = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "write", "release", "flush", "fsync", "create"}, func)
    end })
    route.write.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "open", "read", "write", "release", "flush", "fsync", "create"}, func)
    end

    route.readdir = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end })
    route.readdir.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end

    route.create = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"lookup", "getattr", "create", "open", "read", "write", "release", "flush"}, func)
    end })
    route.create.default = function(func)
        route.register("/{*path}", {"lookup", "getattr", "create", "open", "read", "write", "release", "flush"}, func)
    end

    route.unlink = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"unlink", "lookup", "getattr"}, func)
    end })
    route.unlink.default = function(func)
        route.register("/{*path}", {"unlink", "lookup", "getattr"}, func)
    end

    route.lookup = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "lookup", func)
    end })
    route.lookup.default = function(func)
        route.register("/{*path}", "lookup", func)
    end

    route.getattr = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "getattr", func)
    end })
    route.getattr.default = function(func)
        route.register("/{*path}", "getattr", func)
    end

    route.open = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "open", func)
    end })
    route.open.default = function(func)
        route.register("/{*path}", "open", func)
    end

    route.release = setmetatable({}, { __call = function(_, path, func)
        route.register(path, "release", func)
    end })
    route.release.default = function(func)
        route.register("/{*path}", "release", func)
    end

    route.mkdir = setmetatable({}, { __call = function(_, path, func)
        route.register(path, {"mkdir", "lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end })
    route.mkdir.default = function(func)
        route.register("/{*path}", {"mkdir", "lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end

    route.all = setmetatable({}, { __call = function(_, path, func)
        route.register(path, true, func)
    end })
    route.all.default = function(func)
        route.register("/{*path}", true, func)
    end
end
"#)
            .exec()
            .map_err(|e| format!("route wrappers error: {e}"))?;

        // Execute the user script.
        lua.load(&bytecodes.script)
            .exec()
            .map_err(|e| format!("Lua script error: {e}"))?;

        // Extract functions from registry using known names.
        let extracted = {
            let mut g = funcs.lock().unwrap();
            std::mem::take(&mut *g)
        };
        let default = {
            let mut g = default_handler.lock().unwrap();
            g.take()
        };

        Ok(HandlerRuntime {
            lua,
            funcs: extracted,
            default_handler: default,
        })
    }
}

// ---------------------------------------------------------------------------
// Runtime API setup (extracted for reuse by compile() and from_bytecodes())
// ---------------------------------------------------------------------------

/// Set up all Lua runtime API tables: json.*, yaml.*, toml.*, csv.*, log.*,
/// req.*, oauth.*, store/doc/sql, env.*, fs.*, and CWD helpers.
///
/// Called by both `compile()` (for initial script execution) and
/// `from_bytecodes()` (for worker states).
pub(crate) fn setup_runtime_apis(
    lua: &mlua::Lua,
    cwd: &std::path::Path,
) -> std::result::Result<(), String> {
    // ── Build the `json` table ─────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, val: mlua::Value| {
                serialize::json_encode(lua, val).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("enc", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, val: mlua::Value| {
                serialize::json_encode_pretty(lua, val).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("enc_pretty", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, text: String| {
                serialize::json_decode(lua, text).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("dec", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, (text, path): (String, String)| {
                serialize::json_query(lua, text, path).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("q", fn_).map_err(|e| format!("{e}"))?;

        // json.jq(text, filter) — run a jq filter on JSON text
        {
            let fn_ = lua
                .create_function(|lua, (text, filter): (String, String)| {
                    serialize::json_jq(lua, text, filter).map_err(mlua::Error::runtime)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("jq", fn_).map_err(|e| format!("{e}"))?;
        }

        // json.from_yaml(text) → JSON string, converting YAML to JSON
        {
            let fn_ = lua
                .create_function(|lua, text: String| {
                    crate::serialize::json_from_yaml(lua, text).map_err(mlua::Error::runtime)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("from_yaml", fn_).map_err(|e| format!("{e}"))?;
        }

        lua.globals().set("json", t).map_err(|e| format!("{e}"))?;
    }

    // ── Build the `yaml` table ─────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, val: mlua::Value| {
                serialize::yaml_encode(lua, val).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("enc", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, text: String| {
                serialize::yaml_decode(lua, text).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("dec", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, (text, path): (String, String)| {
                serialize::yaml_query(lua, text, path).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("q", fn_).map_err(|e| format!("{e}"))?;

        // yaml.from_json(text) → YAML string, converting JSON to YAML
        {
            let fn_ = lua
                .create_function(|lua, text: String| {
                    crate::serialize::yaml_from_json(lua, text).map_err(mlua::Error::runtime)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("from_json", fn_).map_err(|e| format!("{e}"))?;
        }

        lua.globals().set("yaml", t).map_err(|e| format!("{e}"))?;
    }

    // ── Build the `toml` table ─────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, val: mlua::Value| {
                serialize::toml_encode(lua, val).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("enc", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, text: String| {
                serialize::toml_decode(lua, text).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("dec", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, (text, path): (String, String)| {
                serialize::toml_query(lua, text, path).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("q", fn_).map_err(|e| format!("{e}"))?;

        lua.globals().set("toml", t).map_err(|e| format!("{e}"))?;
    }

    // ── Build the `csv` table ──────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, val: mlua::Value| {
                serialize::csv_encode(lua, val).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("enc", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, text: String| {
                serialize::csv_decode(lua, text).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("dec", fn_).map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, (text, filter): (String, String)| {
                serialize::csv_query(lua, text, filter).map_err(mlua::Error::runtime)
            })
            .map_err(|e| format!("{e}"))?;
        t.set("q", fn_).map_err(|e| format!("{e}"))?;

        lua.globals().set("csv", t).map_err(|e| format!("{e}"))?;
    }

    // ── Build the `log` table ──────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        // log.print(msg) — print to stderr with prefix
        let fn_ = lua
            .create_function(|_, msg: String| {
                eprintln!("[log.print] {msg}");
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("print", fn_).map_err(|e| format!("{e}"))?;

        // log.debug(msg) — print to stderr with prefix
        let fn_ = lua
            .create_function(|_, msg: String| {
                eprintln!("[log.debug] {msg}");
                Ok(())
            })
            .map_err(|e| format!("{e}"))?;
        t.set("debug", fn_).map_err(|e| format!("{e}"))?;

        lua.globals()
            .set("log", t)
            .map_err(|e| format!("{e}"))?;
    }

    // ── Build the `req` table ──────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        for &method in &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] {
            let method_str = method.to_string();
            let fn_ = lua
                .create_function(
                    move |lua, (url, opts): (String, Option<mlua::Table>)| {
                        crate::req::do_request(lua, &method_str, url, opts)
                            .map_err(mlua::Error::runtime)
                    },
                )
                .map_err(|e| format!("{e}"))?;
            t.set(method.to_lowercase(), fn_)
                .map_err(|e| format!("{e}"))?;
        }

        lua.globals().set("req", t).map_err(|e| format!("{e}"))?;
    }

    // ── Build the `oauth` table ────────────────────────────────────────
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        let fn_ = lua
            .create_function(|lua, _config: mlua::Table| {
                let client = lua.create_table()?;

                let df = lua
                    .create_function(|_, scope: String| {
                        let _ = scope;
                        Ok("Simulated device_flow_start. Requires real OAuth provider."
                            .to_string())
                    })?;
                client.set("device_flow_start", df)?;

                let dp = lua
                    .create_function(|_, (code, interval, max): (String, i64, i64)| {
                        let _ = (code, interval, max);
                        Ok("Simulated device_poll. Requires real OAuth provider.".to_string())
                    })?;
                client.set("device_poll", dp)?;

                let ac = lua
                    .create_function(
                        |_, (endpoint, scope, state): (String, String, String)| {
                            let _ = (endpoint, scope, state);
                            Ok("https://example.com/oauth/authorize?client_id=YOUR_CLIENT_ID&scope=..."
                                .to_string())
                        },
                    )?;
                client.set("auth_code_url", ac)?;

                let ec = lua
                    .create_function(|_, (code, redirect, secret): (String, String, String)| {
                        let _ = (code, redirect, secret);
                        Ok("Simulated exchange_code. Requires real OAuth provider.".to_string())
                    })?;
                client.set("exchange_code", ec)?;

                let at = lua
                    .create_function(|_, (_http_client, _token): (mlua::Table, String)| Ok(true))?;
                client.set("attach_to", at)?;

                Ok(mlua::Value::Table(client))
            })
            .map_err(|e| format!("{e}"))?;
        t.set("client", fn_).map_err(|e| format!("{e}"))?;

        lua.globals()
            .set("oauth", t)
            .map_err(|e| format!("{e}"))?;
    }

    // ── Build the `ninep_client` table (internal 9P2000 client) ──
    {
        let t = lua.create_table().map_err(|e| format!("{e}"))?;

        // ninep_client.read(address, path) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.read_file(&path)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p read: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("read", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.write(address, path, data) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path, data): (String, String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.write_file(&path, &data)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p write: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("write", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.stat(address, path) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.stat(&path)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p stat: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("stat", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.ls(address, path) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.ls(&path, false)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p ls: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("ls", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.lsl(address, path) -> string (long listing)
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.ls(&path, true)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p ls -l: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("lsl", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.mkdir(address, path) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.mkdir(&path)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p mkdir: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("mkdir", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.create(address, path) -> string (touch)
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.create_file(&path)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p create: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("create", fn_).map_err(|e| format!("{e}"))?;
        }

        // ninep_client.remove(address, path) -> string
        {
            let fn_ = lua
                .create_function(|_, (address, path): (String, String)| {
                    let mut client = crate::frontend::ninep_client::NinepClient::connect(&address)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p connect: {e}")))?;
                    client.remove(&path)
                        .map_err(|e| mlua::Error::RuntimeError(format!("9p remove: {e}")))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("remove", fn_).map_err(|e| format!("{e}"))?;
        }

        lua.globals()
            .set("ninep_client", t)
            .map_err(|e| format!("{e}"))?;
    }

    // ── Build the `doc` and `sql` tables ──────────────────────────────
    store::register_lua_apis(lua).map_err(|e| format!("store API: {e}"))?;

    // ── Build the `env` table ─────────────────────────────────────────
    crate::env::register_lua_apis(lua).map_err(|e| format!("env API: {e}"))?;

    // ── Build the `fs` table ──────────────────────────────────────────
    crate::fs::register_lua_apis(lua).map_err(|e| format!("fs API: {e}"))?;

    // ── Set up Lua CWD (for dofile/loadfile/require resolution) ──────
    {
        let cwd_str = cwd.to_string_lossy().to_string();

        // Store CWD in a Lua global so dofile/loadfile wrappers can
        // read it dynamically (and fs.cwd() can update it).
        lua.globals()
            .set("__pinhead_cwd", cwd_str.clone())
            .map_err(|e| format!("{e}"))?;

        // Save original package.path for rebuilding when CWD changes.
        let orig_pp = lua
            .globals()
            .get::<_, mlua::Table>("package")
            .map_err(|e| format!("{e}"))?
            .get::<_, String>("path")
            .map_err(|e| format!("{e}"))?;
        lua.globals()
            .set("__pinhead_orig_path", orig_pp.clone())
            .map_err(|e| format!("{e}"))?;

        // Override dofile, loadfile, and setup package.path.
        lua.load(&format!(
            r#"
do
    local _old_dofile = dofile
    local _old_loadfile = loadfile

    dofile = function(path)
        if type(path) == "string" and path:sub(1,1) ~= "/" then
            path = __pinhead_cwd .. "/" .. path
        end
        return _old_dofile(path)
    end

    loadfile = function(path)
        if type(path) == "string" and path:sub(1,1) ~= "/" then
            path = __pinhead_cwd .. "/" .. path
        end
        return _old_loadfile(path)
    end
end

package.path = "{cwd_str}/?.lua;{cwd_str}/?/init.lua;" .. __pinhead_orig_path
"#,
        ))
        .exec()
        .map_err(|e| format!("CWD setup error: {e}"))?;

        // Add fs.cwd([path]) — getter/setter for Lua's CWD.
        let fn_ = lua
            .create_function(
                |lua, path: Option<String>| -> Result<String, mlua::Error> {
                    match path {
                        Some(p) => {
                            // Resolve relative paths against current CWD.
                            let resolved = if p.starts_with('/') {
                                p.clone()
                            } else {
                                let cur =
                                    lua.globals().get::<_, String>("__pinhead_cwd")?;
                                format!("{cur}/{p}")
                            };

                            // Validate it's a directory.
                            if !std::path::Path::new(&resolved).is_dir() {
                                return Err(mlua::Error::RuntimeError(format!(
                                    "not a directory: {resolved}"
                                )));
                            }

                            // Update __pinhead_cwd.
                            lua.globals()
                                .set("__pinhead_cwd", resolved.as_str())
                                .map_err(|e| {
                                    mlua::Error::RuntimeError(format!(
                                        "failed to set cwd: {e}"
                                    ))
                                })?;

                            // Rebuild package.path with new CWD.
                            let orig = lua
                                .globals()
                                .get::<_, String>("__pinhead_orig_path")?;
                            let new_pp = format!("{resolved}/?.lua;{resolved}/?/init.lua;{orig}");
                            lua.globals()
                                .get::<_, mlua::Table>("package")?
                                .set("path", new_pp)?;

                            Ok(resolved)
                        }
                        None => lua.globals().get::<_, String>("__pinhead_cwd"),
                    }
                },
            )
            .map_err(|e| format!("{e}"))?;

        let fs_table = lua
            .globals()
            .get::<_, mlua::Table>("fs")
            .map_err(|e| format!("{e}"))?;
        fs_table.set("cwd", fn_).map_err(|e| format!("{e}"))?;
    }

    Ok(())
}
