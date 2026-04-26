mod fsop;
mod handler;
mod frontend;
mod req;
mod serialize;
mod store;

mod router; // keep after handler for types

use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::LocalSet;

use handler::HandlerRequest;
use router::RouteMeta;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    eprintln!("=== pinhead: filesystem path router (Lua) ===");

    // ── 1. Load Lua script ──────────────────────────────────────────────
    let script = load_script();

    // ── 2. Lua setup (synchronous, on main thread) ──────────────────────
    let (cfg, routes, runtime) =
        handler::HandlerRuntime::new(&script).expect("Lua setup failed");

    if routes.is_empty() {
        eprintln!("[main] WARNING: no routes registered — add route.register() calls to your script");
    } else {
        eprintln!("[main] loaded {} route(s) from Lua", routes.len());
        for r in &routes {
            let ops_str = if r.ops.is_empty() {
                "(all)".to_string()
            } else {
                r.ops.join(",")
            };
            eprintln!("[main]   {}  [{}]  →  '{}'", r.pattern, ops_str, r.handler_name);
        }
    }

    // ── 3. Build the path router from Lua-registered routes ─────────────
    // Merge registrations by pattern — each pattern gets a RouteMeta with
    // an op→handler_name map.
    let mut pattern_map: HashMap<&str, RouteMeta> = HashMap::new();
    for r in &routes {
        let meta = pattern_map
            .entry(&r.pattern)
            .or_insert_with(|| RouteMeta {
                handlers: HashMap::new(),
            });
        if r.ops.is_empty() {
            meta.handlers
                .insert("*".to_string(), r.handler_name.clone());
        } else {
            for op in &r.ops {
                meta.handlers
                    .insert(op.clone(), r.handler_name.clone());
            }
        }
    }

    let mut rb = router::new();
    for (pattern, meta) in &pattern_map {
        rb.register(*pattern, meta.clone())
            .unwrap_or_else(|e| panic!("route `{}`: {e}", pattern));
    }

    let (handler_tx, handler_rx) = mpsc::channel::<HandlerRequest>(64);
    let (frontend_tx, router_h) = rb.build(handler_tx);

    // ── 4. Spawn the !Send Lua handler inside a LocalSet ───────────────
    let local = LocalSet::new();
    local.spawn_local(runtime.run(handler_rx));

    // ── 5. Spawn the router task (Send) ─────────────────────────────────
    let router_h = tokio::spawn(router_h);

    // ── 6. Spawn frontends based on config ──────────────────────────────
    let has_config = !cfg.fuse_mounts.is_empty()
        || !cfg.ninep_listeners.is_empty()
        || !cfg.sshfs_listeners.is_empty();

    if has_config {
        for path in &cfg.fuse_mounts {
            eprintln!("[main] FUSE mount: {path} (TODO: real FUSE daemon)");
        }

        for listener in &cfg.ninep_listeners {
            let tx = frontend_tx.clone();
            if let Some(path) = listener.strip_prefix("sock:") {
                let path = path.to_string();
                tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve(tx, &path).await {
                        eprintln!("[9p-socket] error: {e}");
                    }
                });

            } else if let Some(_addr) = listener.strip_prefix("tcp:") {
                let addr = _addr.to_string();
                tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve_tcp(tx, &addr).await {
                        eprintln!("[9p-tcp] error: {e}");
                    }
                });
            } else if let Some(addr) = listener.strip_prefix("udp:") {
                let addr = addr.to_string();
                tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve_udp(tx, &addr).await {
                        eprintln!("[9p-udp] error: {e}");
                    }
                });
            } else {
                eprintln!("[main] unknown 9p listener prefix: {listener}");
            }
        }

        for listener in &cfg.sshfs_listeners {
            let tx = frontend_tx.clone();
            let addr = listener.to_string();
            let sshfs_cfg = frontend::sshfs::SshfsConfig {
                password: cfg.sshfs_password.clone(),
                authorized_keys_path: cfg.sshfs_authorized_keys_path.clone(),
                userpasswds: cfg.sshfs_userpasswds.clone(),
            };
            tokio::spawn(async move {
                if let Err(e) = frontend::sshfs::serve(tx, &addr, sshfs_cfg).await {
                    eprintln!("[sshfs] error: {e}");
                }
            });
        }
    }

    // ── 7. Run everything concurrently ──────────────────────────────────
    tokio::select! {
        _ = local     => eprintln!("[main] Lua handler exited"),
        _ = router_h  => eprintln!("[main] router task exited"),
    }

    drop(frontend_tx);
    // Brief pause for in-flight 9P responses.
    tokio::time::sleep(Duration::from_millis(100)).await;

    eprintln!("=== pinhead: done ===");
}

// ── Script loading ─────────────────────────────────────────────────────────

fn load_script() -> String {
    // Check CLI argument.
    if let Some(path) = std::env::args().nth(1) {
        if !path.starts_with('-') {
            match std::fs::read_to_string(&path) {
                Ok(s) => return s,
                Err(e) => {
                    eprintln!("[main] warning: cannot read `{path}`: {e}")
                }
            }
        }
    }

    // Fallback: look for pinhead.lua in CWD.
    if let Ok(s) = std::fs::read_to_string("pinhead.lua") {
        return s;
    }

    eprintln!("[main] error: no script found — provide a .lua file as argument or place pinhead.lua in CWD");
    std::process::exit(1);
}


