use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use rlua::{Lua, RegistryKey, Value};
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

// ---------------------------------------------------------------------------
// Lua handler runtime — !Send, must run on one thread via LocalSet
// ---------------------------------------------------------------------------

/// The Lua runtime state.  Holds the Lua VM and the registered function keys.
/// `!Send` — must be used via `spawn_local` inside a `tokio::task::LocalSet`.
pub struct HandlerRuntime {
    lua: Lua,
    funcs: HashMap<String, RegistryKey>,
    default_handler: Option<RegistryKey>,
}

impl HandlerRuntime {
    /// Create a new Lua VM, load `script`, and register all routes declared
    /// by the script.  Runs synchronously — call before entering the async
    /// section of `main`.
    ///
    /// The script uses `route.register(pattern, ops, func)` and
    /// `route.default(func)` to declare routes, and `fuse.*()` / `ninep.*()`
    /// setters for frontend configuration.
    pub fn new(script: &str) -> Result<(Config, Vec<RouteRegistration>, Self), String> {
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
                        move |lua, (pattern, ops_val, func): (String, rlua::Value, rlua::Function)| {
                            let name =
                                format!("__route_{}", routes.lock().unwrap().len());
                            let key = lua.create_registry_value(func)?;

                            // Parse ops: nil → all ops, string → single op,
                            // table → set of op strings.
                            let ops: Vec<String> = match &ops_val {
                                rlua::Value::Nil | rlua::Value::Boolean(true) => Vec::new(),
                                rlua::Value::String(s) => {
                                    vec![s.to_str()?.to_string()]
                                }
                                rlua::Value::Table(t) => {
                                    let mut v = Vec::new();
                                    for pair in t.clone().pairs::<rlua::Value, rlua::Value>() {
                                        let (_, val) = pair?;
                                        if let rlua::Value::String(s) = val {
                                            v.push(s.to_str()?.to_string());
                                        }
                                    }
                                    v
                                }
                                _ => {
                                    return Err(rlua::Error::RuntimeError(
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
                    .create_function(move |lua, func: rlua::Function| {
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
    function route.read(path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "release", "flush"}, func)
    end
    function route.write(path, func)
        route.register(path, {"lookup", "getattr", "open", "read", "write", "release", "flush", "fsync"}, func)
    end
    function route.readdir(path, func)
        route.register(path, {"lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end
    function route.create(path, func)
        route.register(path, {"lookup", "getattr", "create", "open", "read", "write", "release", "flush"}, func)
    end
    function route.unlink(path, func)
        route.register(path, {"unlink", "lookup", "getattr"}, func)
    end
    function route.lookup(path, func)
        route.register(path, "lookup", func)
    end
    function route.getattr(path, func)
        route.register(path, "getattr", func)
    end
    function route.open(path, func)
        route.register(path, "open", func)
    end
    function route.release(path, func)
        route.register(path, "release", func)
    end
    function route.mkdir(path, func)
        route.register(path, {"mkdir", "lookup", "getattr", "opendir", "readdir", "releasedir"}, func)
    end
    function route.all(path, func)
        route.register(path, true, func)
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

        // ── Build the `json` table ─────────────────────────────────────────
        {
            let t = lua.create_table().map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, val: rlua::Value| {
                    serialize::json_encode(lua, val).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("enc", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, val: rlua::Value| {
                    serialize::json_encode_pretty(lua, val).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("enc_pretty", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, text: String| {
                    serialize::json_decode(lua, text).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("dec", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, (text, path): (String, String)| {
                    serialize::json_query(lua, text, path).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("q", fn_).map_err(|e| format!("{e}"))?;

            // json.jq(text, filter) — run a jq filter on JSON text
            {
                let fn_ = lua
                    .create_function(|lua, (text, filter): (String, String)| {
                        serialize::json_jq(lua, text, filter)
                            .map_err(rlua::Error::RuntimeError)
                    })
                    .map_err(|e| format!("{e}"))?;
                t.set("jq", fn_).map_err(|e| format!("{e}"))?;
            }

            // json.from_yaml(text) → JSON string, converting YAML to JSON
            {
                let fn_ = lua
                    .create_function(|lua, text: String| {
                        crate::serialize::json_from_yaml(lua, text)
                            .map_err(rlua::Error::RuntimeError)
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
                .create_function(|lua, val: rlua::Value| {
                    serialize::yaml_encode(lua, val).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("enc", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, text: String| {
                    serialize::yaml_decode(lua, text).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("dec", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, (text, path): (String, String)| {
                    serialize::yaml_query(lua, text, path).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("q", fn_).map_err(|e| format!("{e}"))?;

            // yaml.from_json(text) → YAML string, converting JSON to YAML
            {
                let fn_ = lua
                    .create_function(|lua, text: String| {
                        crate::serialize::yaml_from_json(lua, text)
                            .map_err(rlua::Error::RuntimeError)
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
                .create_function(|lua, val: rlua::Value| {
                    serialize::toml_encode(lua, val).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("enc", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, text: String| {
                    serialize::toml_decode(lua, text).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("dec", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, (text, path): (String, String)| {
                    serialize::toml_query(lua, text, path).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("q", fn_).map_err(|e| format!("{e}"))?;

            lua.globals().set("toml", t).map_err(|e| format!("{e}"))?;
        }

        // ── Build the `csv` table ──────────────────────────────────────────
        {
            let t = lua.create_table().map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, val: rlua::Value| {
                    serialize::csv_encode(lua, val).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("enc", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, text: String| {
                    serialize::csv_decode(lua, text).map_err(rlua::Error::RuntimeError)
                })
                .map_err(|e| format!("{e}"))?;
            t.set("dec", fn_).map_err(|e| format!("{e}"))?;

            let fn_ = lua
                .create_function(|lua, (text, filter): (String, String)| {
                    serialize::csv_query(lua, text, filter).map_err(rlua::Error::RuntimeError)
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
                    .create_function(move |lua, (url, opts): (String, Option<rlua::Table>)| {
                        crate::req::do_request(lua, &method_str, url, opts)
                            .map_err(rlua::Error::RuntimeError)
                    })
                    .map_err(|e| format!("{e}"))?;
                t.set(method.to_lowercase(), fn_)
                    .map_err(|e| format!("{e}"))?;
            }

            lua.globals().set("req", t).map_err(|e| format!("{e}"))?;
        }

        // ── Build the `oauth` table ────────────────────────────────────────
        {
            let t = lua.create_table().map_err(|e| format!("{e}"))?;

            // oauth.client(config) — returns a client table with method stubs
            let fn_ = lua
                .create_function(|lua, _config: rlua::Table| {
                    let client = lua.create_table()?;

                    let df = lua
                        .create_function(|_, scope: String| {
                            let _ = scope;
                            Ok(
                                "Simulated device_flow_start. Requires real OAuth provider."
                                    .to_string(),
                            )
                        })?;
                    client.set("device_flow_start", df)?;

                    let dp = lua
                        .create_function(|_, (code, interval, max): (String, i64, i64)| {
                            let _ = (code, interval, max);
                            Ok(
                                "Simulated device_poll. Requires real OAuth provider."
                                    .to_string(),
                            )
                        })?;
                    client.set("device_poll", dp)?;

                    let ac = lua
                        .create_function(
                            |_, (endpoint, scope, state): (String, String, String)| {
                                let _ = (endpoint, scope, state);
                                Ok(
                                    "https://example.com/oauth/authorize?client_id=YOUR_CLIENT_ID&scope=..."
                                        .to_string(),
                                )
                            },
                        )?;
                    client.set("auth_code_url", ac)?;

                    let ec = lua
                        .create_function(
                            |_, (code, redirect, secret): (String, String, String)| {
                                let _ = (code, redirect, secret);
                                Ok(
                                    "Simulated exchange_code. Requires real OAuth provider."
                                        .to_string(),
                                )
                            },
                        )?;
                    client.set("exchange_code", ec)?;

                    let at = lua
                        .create_function(|_, (_http_client, _token): (rlua::Table, String)| {
                            Ok(true)
                        })?;
                    client.set("attach_to", at)?;

                    Ok(rlua::Value::Table(client))
                })
                .map_err(|e| format!("{e}"))?;
            t.set("client", fn_).map_err(|e| format!("{e}"))?;

            lua.globals()
                .set("oauth", t)
                .map_err(|e| format!("{e}"))?;
        }

        // ── Build the `doc` and `sql` tables ──────────────────────────────
        store::register_lua_apis(&lua).map_err(|e| format!("store API: {e}"))?;

        // ── Build the `env` table ─────────────────────────────────────────
        crate::env::register_lua_apis(&lua).map_err(|e| format!("env API: {e}"))?;

        // ── Build the `fs` table ──────────────────────────────────────────
        crate::fs::register_lua_apis(&lua).map_err(|e| format!("fs API: {e}"))?;

        // Execute the user script.
        lua.load(script)
            .exec()
            .map_err(|e| format!("Lua script error: {e}"))?;

        // Extract route data.
        let registered = {
            let mut g = routes.lock().unwrap();
            std::mem::take(&mut *g)
        };
        let funcs = {
            let mut g = funcs.lock().unwrap();
            std::mem::take(&mut *g)
        };
        let default = {
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

        Ok((
            cfg,
            registered,
            Self {
                lua,
                funcs,
                default_handler: default,
            },
        ))
    }

    /// Async handler loop.  Call via `spawn_local` inside a `LocalSet`.
    ///
    /// Receives requests, dispatches to the registered Lua function, and
    /// sends the response back through each request's oneshot channel.
    pub async fn run(
        self,
        mut rx: tokio::sync::mpsc::Receiver<HandlerRequest>,
    ) {
        // We use `recv()` (not `blocking_recv()`) because this runs inside
        // the tokio async runtime via `spawn_local`.
        while let Some(req) = rx.recv().await {
            let result = self.call_lua(&req);
            let _ = req.reply.send(result);
        }
        eprintln!("[lua] request channel closed, shutting down handler");
    }

    /// Call the matching Lua function for a single request.
    fn call_lua(&self, req: &HandlerRequest) -> Result<HandlerResponse, String> {
        // Find the registered function.
        let key = self.funcs.get(&req.handler_name);
        let func = match key {
            Some(key) => self
                .lua
                .registry_value::<rlua::Function>(key)
                .map_err(|e| {
                    format!("failed to get Lua function `{}`: {e}", req.handler_name)
                })?,
            None => match self.default_handler.as_ref() {
                Some(key) => self
                    .lua
                    .registry_value::<rlua::Function>(key)
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
        })
    }
}
