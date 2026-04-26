mod env;
mod fs;
mod fsop;
mod handler;
mod frontend;
mod req;
mod serialize;
mod store;

mod router; // keep after handler for types

use std::collections::HashMap;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::task::LocalSet;

use handler::HandlerRequest;
use router::RouteMeta;

#[tokio::main]
async fn main() {
    // ── 1. Load Lua script ──────────────────────────────────────────────
    let script = load_script();

    eprintln!(">> pinhead: a dynamic virtual filesystem fueled by Lua\n");

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
    let mut frontend_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut fuse_sessions: Vec<_> = Vec::new();
    let has_config = !cfg.fuse_mounts.is_empty()
        || !cfg.ninep_listeners.is_empty()
        || !cfg.sshfs_listeners.is_empty();

    eprintln!("[debug] has_config={has_config} fuse_mounts={} ninep={} sshfs={}",
        cfg.fuse_mounts.len(), cfg.ninep_listeners.len(), cfg.sshfs_listeners.len());

    if has_config {
        for path in &cfg.fuse_mounts {
            let tx = frontend_tx.clone();
            let path = path.clone();
            eprintln!("[main] FUSE mount: {path}");
            match frontend::fuse::mount(tx, &path) {
                Ok(bg) => fuse_sessions.push(bg),
                Err(e) => eprintln!("[fuse] mount error: {e}"),
            }
        }

        for listener in &cfg.ninep_listeners {
            let tx = frontend_tx.clone();
            if let Some(path) = listener.strip_prefix("sock:") {
                let path = path.to_string();
                let h = tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve(tx, &path).await {
                        eprintln!("[9p-socket] error: {e}");
                    }
                });
                frontend_handles.push(h);

            } else if let Some(_addr) = listener.strip_prefix("tcp:") {
                let addr = _addr.to_string();
                let h = tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve_tcp(tx, &addr).await {
                        eprintln!("[9p-tcp] error: {e}");
                    }
                });
                frontend_handles.push(h);
            } else if let Some(addr) = listener.strip_prefix("udp:") {
                let addr = addr.to_string();
                let h = tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve_udp(tx, &addr).await {
                        eprintln!("[9p-udp] error: {e}");
                    }
                });
                frontend_handles.push(h);
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
            let h = tokio::spawn(async move {
                if let Err(e) = frontend::sshfs::serve(tx, &addr, sshfs_cfg).await {
                    eprintln!("[sshfs] error: {e}");
                }
            });
            frontend_handles.push(h);
        }
    }

    // ── 7. Run everything concurrently ──────────────────────────────────
    let mut sigint =
        signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
    tokio::select! {
        _ = local     => eprintln!("[main] Lua handler exited"),
        _ = router_h  => eprintln!("[main] router task exited"),
        _ = sigint.recv() => eprintln!("[main] received SIGINT, shutting down"),
    }

    // ── 8. Cleanup: kill frontends, unmount, remove sockets ────────────
    drop(frontend_tx);

    for path in &cfg.fuse_mounts {
        eprintln!("[main] FUSE unmount: {path} (auto-unmount on BackgroundSession drop)");
    }

    for listener in &cfg.ninep_listeners {
        eprintln!("[main] 9p kill: {listener}");
    }

    for listener in &cfg.sshfs_listeners {
        eprintln!("[main] ssh kill: {listener}");
    }

    // Abort all frontend tasks — this drops their futures, which triggers
    // cleanup guards (e.g. SocketCleanup in ninep serve removes socket files).
    for h in frontend_handles {
        h.abort();
    }

    // FUSE sessions: drop triggers auto-unmount (BackgroundSession).
    drop(fuse_sessions);

    // Brief pause for in-flight responses and cleanup guards.
    tokio::time::sleep(Duration::from_millis(100)).await;
    eprintln!("\n>> pinhead: done");
}

// ── Script loading ─────────────────────────────────────────────────────────

fn load_script() -> String {
    // 1. CLI argument (covers shebang invocation and direct invocation).
    if let Some(path) = std::env::args().nth(1) {
        if !path.starts_with('-') {
            match std::fs::read_to_string(&path) {
                Ok(s) => return s,
                Err(e) => {
                    eprintln!("[main] error: cannot read `{path}`: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    // 2. Piped input — read from stdin if it is not a terminal.
    use std::io::{IsTerminal, Read};
    if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        let n = std::io::stdin()
            .read_to_string(&mut s)
            .unwrap_or_else(|e| {
                eprintln!("[main] error: reading stdin: {e}");
                std::process::exit(1);
            });
        if n > 0 {
            return s;
        }
    }

    eprintln!(
        "\nUsage:\n\
         \n\x20\x20pinhead SCRIPT.lua\n\
         \x20\x20#!/path/to/pinhead\n\
         \x20\x20cat SCRIPT.lua|pinhead\n"
    );
    std::process::exit(1);
}


