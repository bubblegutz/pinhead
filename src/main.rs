mod fsop;
mod handler;
mod frontend;
mod serialize;

mod router; // keep after handler for types

use std::collections::HashMap;
use std::time::Duration;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use fsop::FsOperation;
use handler::HandlerRequest;
use router::{Request, RouteMeta};

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
            tokio::spawn(run_fuse_frontend(frontend_tx.clone()));
        }

        for listener in &cfg.ninep_listeners {
            let tx = frontend_tx.clone();
            if let Some(path) = listener.strip_prefix("sock:") {
                let path = path.to_string();
                let client_path = path.clone();
                tokio::spawn(async move {
                    if let Err(e) = frontend::ninep::serve(tx, &path).await {
                        eprintln!("[9p-socket] error: {e}");
                    }
                });
                tokio::spawn(async move {
                    run_ninep_client(&client_path).await;
                });
            } else if let Some(addr) = listener.strip_prefix("tcp:") {
            } else if let Some(addr) = listener.strip_prefix("tcp:") {
                let addr = addr.to_string();
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
    } else {
        eprintln!("[main] no frontend config — add ninep.listen(...), fuse.mount(...), or sshfs.listen(...) to your script");
        tokio::spawn(run_fuse_frontend(frontend_tx.clone()));
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

    // Last fallback: compiled-in demo script.
    eprintln!("[main] no script found, using built-in demo");
    include_str!("../examples/handler.lua").to_string()
}

// ── Simulated FUSE frontend ────────────────────────────────────────────────

async fn run_fuse_frontend(frontend_tx: mpsc::Sender<Request>) {
    let ops = [
        (FsOperation::Lookup, "/"),
        (FsOperation::GetAttr, "/users/42/profile"),
        (FsOperation::Read, "/users/42/profile"),
        (FsOperation::Read, "/files/readme.txt"),
        (FsOperation::Lookup, "/nonexistent"),
    ];

    for (op, path) in &ops {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: *op,
            path: path.to_string(),
            data: Bytes::new(),
            reply: reply_tx,
        };

        if frontend_tx.send(req).await.is_err() {
            break;
        }

        match reply_rx.await {
            Ok(Ok(resp)) => {
                let text = String::from_utf8_lossy(&resp.data);
                eprintln!("[FUSE] {} {} → {text:?}", op.as_str(), path);
            }
            Ok(Err(e)) => eprintln!("[FUSE] {} {} → ERR {e}", op.as_str(), path),
            Err(_) => eprintln!("[FUSE] {} {} → reply lost", op.as_str(), path),
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    eprintln!("[FUSE] frontend done");
}

// ── 9P demo client ────────────────────────────────────────────────────────

async fn run_ninep_client(socket_path: &str) {
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut stream = match UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[9p-client] connect error: {e}");
            return;
        }
    };

    // Tversion
    let tversion = build_9p_message(100, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&8192u32.to_le_bytes());
        b.extend_from_slice(&6u16.to_le_bytes());
        b.extend_from_slice(b"9P2000");
        b
    });
    stream.write_all(&tversion).await.unwrap();
    let _ = read_9p_reply(&mut stream).await;
    eprintln!("[9p-client] version negotiated");

    // Tattach
    let tattach = build_9p_message(104, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.push(b'u');
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    });
    stream.write_all(&tattach).await.unwrap();
    let _ = read_9p_reply(&mut stream).await;
    eprintln!("[9p-client] attach done");

    // Twalk root → users/42/profile
    let target = "users/42/profile";
    let twalk = build_9p_message(110, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        let parts: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
        b.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        for part in &parts {
            b.extend_from_slice(&(part.len() as u16).to_le_bytes());
            b.extend_from_slice(part.as_bytes());
        }
        b
    });
    stream.write_all(&twalk).await.unwrap();
    let resp = read_9p_reply(&mut stream).await;
    eprintln!("[9p-client] walk '{}': {} bytes", target, resp.len());

    // Topen
    let topen = build_9p_message(112, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.push(0);
        b
    });
    stream.write_all(&topen).await.unwrap();
    let _ = read_9p_reply(&mut stream).await;
    eprintln!("[9p-client] open done");

    // Tread
    let tread = build_9p_message(116, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&4096u32.to_le_bytes());
        b
    });
    stream.write_all(&tread).await.unwrap();
    let resp = read_9p_reply(&mut stream).await;
    if resp.len() >= 4 {
        let data_len = u32::from_le_bytes(resp[0..4].try_into().unwrap()) as usize;
        let data = &resp[4..4 + data_len.min(resp.len() - 4)];
        let text = String::from_utf8_lossy(data);
        eprintln!("[9p-client] read: {text:?}");
    }

    // Tclunk
    let tclunk = build_9p_message(120, 0, &{
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes());
        b
    });
    stream.write_all(&tclunk).await.unwrap();
    let _ = read_9p_reply(&mut stream).await;
    eprintln!("[9p-client] clunk done");

    eprintln!("[9p-client] frontend done");
}

fn build_9p_message(msg_type: u8, tag: u16, body: &[u8]) -> Vec<u8> {
    let size = 7 + body.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(body);
    buf
}

async fn read_9p_reply(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 7];
    if stream.read_exact(&mut header).await.is_err() {
        return Vec::new();
    }
    let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let body_len = size.saturating_sub(7);
    let mut body = vec![0u8; body_len];
    if body_len > 0 {
        let _ = stream.read_exact(&mut body).await;
    }
    body
}
