//! Minimal 9P2000 server frontend backed by the pinhead path router.
//!
//! Implements a 9P2000 wire protocol server over Unix sockets.
//! Session state (fid/qid tracking) is managed internally.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

use crate::fsop::FsOperation;
use crate::router::Request;

// ── Socket cleanup guard ─────────────────────────────────────────────
/// Removes the socket file on drop (i.e. when the server future is
/// cancelled or the function returns).
struct SocketCleanup {
    path: String,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── 9P2000 message types ──────────────────────────────────────────────
const TVERSION: u8 = 100;
const RVERSION: u8 = 101;
const TATTACH: u8 = 104;
const RATTACH: u8 = 105;
const TFLUSH: u8 = 108;
const RFLUSH: u8 = 109;
const TWALK: u8 = 110;
const RWALK: u8 = 111;
const TOPEN: u8 = 112;
const ROPEN: u8 = 113;
#[expect(dead_code, reason = "9P spec — Create/WStat not yet dispatched")]
const TCREATE: u8 = 114;
#[expect(dead_code)]
const RCREATE: u8 = 115;
const TREAD: u8 = 116;
const RREAD: u8 = 117;
const TWRITE: u8 = 118;
const RWRITE: u8 = 119;
const TCLUNK: u8 = 120;
const RCLUNK: u8 = 121;
const TREMOVE: u8 = 122;
const RREMOVE: u8 = 123;
const TSTAT: u8 = 124;
const RSTAT: u8 = 125;
#[expect(dead_code, reason = "9P spec — WStat not yet dispatched")]
const TWSTAT: u8 = 126;
#[expect(dead_code)]
const RWSTAT: u8 = 127;

// ── Qid ───────────────────────────────────────────────────────────────
#[derive(Clone, Copy, Debug, Default)]
struct Qid {
    ty: u8,
    version: u32,
    path: u64,
}

fn encode_qid(buf: &mut Vec<u8>, qid: Qid) {
    buf.push(qid.ty);
    buf.extend_from_slice(&qid.version.to_le_bytes());
    buf.extend_from_slice(&qid.path.to_le_bytes());
}

// ── Stat (directory entry) ────────────────────────────────────────────
fn encode_stat(buf: &mut Vec<u8>, name: &str, qid: Qid, length: u64, is_dir: bool) {
    // Build the stat body first to compute size.
    let mut body = Vec::new();

    let mode = if is_dir { 0x800001edu32 } else { 0x1edu32 };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    body.extend_from_slice(&0u16.to_le_bytes()); // type
    body.extend_from_slice(&0u32.to_le_bytes()); // dev
    encode_qid(&mut body, qid);
    body.extend_from_slice(&mode.to_le_bytes());
    body.extend_from_slice(&now.to_le_bytes()); // atime
    body.extend_from_slice(&now.to_le_bytes()); // mtime
    body.extend_from_slice(&length.to_le_bytes());
    // short string: 2-byte length + data
    body.extend_from_slice(&(name.len() as u16).to_le_bytes());
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&2u16.to_le_bytes()); // uid
    body.extend_from_slice(b"0");
    body.extend_from_slice(&2u16.to_le_bytes()); // gid
    body.extend_from_slice(b"0");
    body.extend_from_slice(&2u16.to_le_bytes()); // muid
    body.extend_from_slice(b"0");

    let size = body.len() as u16;
    buf.extend_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&body);
}

// ── Fid / Qid state ───────────────────────────────────────────────────
struct NinepState {
    fids: HashMap<u32, FidEntry>,
    paths: HashMap<u64, PathEntry>,
    next_qid: u64,
}

#[derive(Clone)]
struct FidEntry {
    qid: u64,
    path: String,
    open: bool,
    is_dir: bool,
}

#[derive(Clone)]
struct PathEntry {
    path: String,
    is_dir: bool,
}

impl NinepState {
    fn new() -> Self {
        let mut paths = HashMap::new();
        paths.insert(
            0,
            PathEntry {
                path: "/".into(),
                is_dir: true,
            },
        );
        Self {
            fids: HashMap::new(),
            paths,
            next_qid: 1,
        }
    }

    fn alloc_qid(&mut self, path: &str, is_dir: bool) -> u64 {
        // Check if path already has a qid
        for (&qid, entry) in &self.paths {
            if entry.path == path {
                return qid;
            }
        }
        let qid = self.next_qid;
        self.next_qid += 1;
        self.paths.insert(
            qid,
            PathEntry {
                path: path.to_string(),
                is_dir,
            },
        );
        qid
    }

    fn set_fid(&mut self, fid: u32, qid: u64, path: String, is_dir: bool) {
        self.fids.insert(
            fid,
            FidEntry {
                qid,
                path,
                open: false,
                is_dir,
            },
        );
    }
}

// ── Shared frontend ───────────────────────────────────────────────────
struct Shared {
    state: Mutex<NinepState>,
    router_tx: mpsc::Sender<Request>,
    msize: RwLock<u32>,
}

/// Start a 9P2000 server on the given Unix socket path.
///
/// For each incoming connection, a new task is spawned to handle the 9P
/// protocol, forwarding filesystem operations to the pinhead router.
pub async fn serve(
    router_tx: mpsc::Sender<Request>,
    socket_path: &str,
) -> std::io::Result<()> {
    // Remove stale socket.
    let _ = tokio::fs::remove_file(socket_path).await;
    let listener = UnixListener::bind(socket_path)?;
    eprintln!("[9p] listening on {socket_path}");

    // SocketCleanup runs Drop when this future is cancelled/aborted,
    // ensuring the socket file is removed on exit.
    let _cleanup = SocketCleanup {
        path: socket_path.to_string(),
    };

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(8192),
    });

    loop {
        let (mut stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            let buf = vec![0u8; 65536];
            loop {
                // Read 7-byte fixed header
                let mut header = [0u8; 7];
                match NinepStream::read_exact(&mut stream, &mut header).await {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => {
                        eprintln!("[9p] read error: {e}");
                        break;
                    }
                }

                let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                let msg_type = header[4];
                let tag = u16::from_le_bytes(header[5..7].try_into().unwrap());

                // Read the rest of the message body
                let body_len = size.saturating_sub(7);
                if body_len > buf.len() {
                    eprintln!("[9p] message too large: {body_len}");
                    break;
                }

                let mut body = vec![0u8; body_len];
                if body_len > 0 {
                    if let Err(e) = NinepStream::read_exact(&mut stream, &mut body).await {
                        eprintln!("[9p] read body error: {e}");
                        break;
                    }
                }

                if let Err(e) = handle_message(&shared, &mut stream, msg_type, tag, &body).await {
                    eprintln!("[9p] handler error: {e}");
                    // Send Rlerror or just drop
                    send_error(&mut stream, tag, &e).await;
                }
            }
            let _ = NinepStream::shutdown(&mut stream).await;
        });
    }
}

// ── Message dispatch ──────────────────────────────────────────────────
async fn handle_message(
    shared: &Shared,
    stream: &mut impl NinepStream,
    msg_type: u8,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    match msg_type {
        TVERSION => handle_version(shared, stream, tag, body).await,
        TATTACH => handle_attach(shared, stream, tag, body).await,
        TWALK => handle_walk(shared, stream, tag, body).await,
        TOPEN => handle_open(shared, stream, tag, body).await,
        TREAD => handle_read(shared, stream, tag, body).await,
        TWRITE => handle_write(shared, stream, tag, body).await,
        TCLUNK => handle_clunk(shared, stream, tag, body).await,
        TREMOVE => handle_remove(shared, stream, tag, body).await,
        TSTAT => handle_stat(shared, stream, tag, body).await,
        TFLUSH => handle_flush(stream, tag).await,
        _ => Err(format!("unknown message type: {msg_type}")),
    }
}

async fn send_reply(stream: &mut impl NinepStream, msg_type: u8, tag: u16, body: &[u8]) {
    let size = 7 + body.len();
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(body);
    let _ = stream.write_all(&buf).await;
}

async fn send_error(stream: &mut impl NinepStream, tag: u16, err: &str) {
    let bytes = err.as_bytes();
    let len = bytes.len().min(65535) as u16;
    let mut body = Vec::with_capacity(2 + len as usize);
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(&bytes[..len as usize]);
    send_reply(stream, RERROR, tag, &body).await;
}

// ── Handlers ──────────────────────────────────────────────────────────
async fn handle_version(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: msize(u32), version(s)
    if body.len() < 4 {
        return Err("short version message".into());
    }
    let msize = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _version = if body.len() > 4 {
        let vlen = u16::from_le_bytes(body[4..6].try_into().unwrap()) as usize;
        if 6 + vlen <= body.len() {
            String::from_utf8_lossy(&body[6..6 + vlen]).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let msize = msize.min(65536).max(4096);
    *shared.msize.write().await = msize;

    let mut reply = Vec::new();
    reply.extend_from_slice(&msize.to_le_bytes());
    // version string "9P2000"
    reply.extend_from_slice(&6u16.to_le_bytes());
    reply.extend_from_slice(b"9P2000");

    send_reply(stream, RVERSION, tag, &reply).await;
    Ok(())
}

async fn handle_attach(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), afid(u32), uname(s), aname(s)
    if body.len() < 8 {
        return Err("short attach message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _afid = u32::from_le_bytes(body[4..8].try_into().unwrap());
    // Skip strings for now

    // Register root fid
    let mut state = shared.state.lock().await;
    state.set_fid(fid, 0, "/".into(), true);

    let qid = Qid {
        ty: 0x80, // QTDIR
        version: 0,
        path: 0,
    };
    let mut reply = Vec::new();
    encode_qid(&mut reply, qid);
    send_reply(stream, RATTACH, tag, &reply).await;
    Ok(())
}

async fn handle_walk(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), newfid(u32), nwname(u16), [wname(s) ...]
    if body.len() < 9 {
        return Err("short walk message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let new_fid = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let nwname = u16::from_le_bytes(body[8..10].try_into().unwrap()) as usize;

    let parent_path;
    let parent_qid;
    let parent_is_dir;
    {
        let state = shared.state.lock().await;
        let parent = state.fids.get(&fid).ok_or("unknown fid")?.clone();
        parent_path = parent.path;
        parent_qid = parent.qid;
        parent_is_dir = parent.is_dir;
    }

    let mut cur_path = parent_path;
    let mut cur_qid = parent_qid;
    let mut wqids = Vec::new();
    let mut offset = 10;
    let mut is_dir = parent_is_dir;

    for i in 0..nwname {
        if offset + 2 > body.len() {
            return Err("short walk wname".into());
        }
        let nlen = u16::from_le_bytes(body[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        if offset + nlen > body.len() {
            return Err("short walk wname data".into());
        }
        let child = String::from_utf8_lossy(&body[offset..offset + nlen]).to_string();
        offset += nlen;

        // Build the full path and route it
        let full_path = if cur_path.ends_with('/') {
            format!("{cur_path}{child}")
        } else {
            format!("{cur_path}/{child}")
        };

        // Only validate the final segment through the router — intermediate
        // segments may match parameterized routes (e.g. /user/{name}) and
        // aren't complete paths the router can resolve.
        let is_last = i + 1 == nwname;
        if is_last {
            // Send Lookup to router to verify path exists
            let (reply_tx, reply_rx) = oneshot::channel();
            let req = Request {
                op: FsOperation::Lookup,
                path: full_path.clone(),
                data: Bytes::new(),
                reply: reply_tx,
            };
            shared
                .router_tx
                .send(req)
                .await
                .map_err(|_| "router gone".to_string())?;
            reply_rx
                .await
                .map_err(|_| "handler gone".to_string())?
                .map_err(|e| format!("lookup failed: {e}"))?;
        }

        // Allocate qid
        is_dir = child.is_empty() || child == "." || child == "..";
        let mut state = shared.state.lock().await;
        cur_qid = state.alloc_qid(&full_path, is_dir);
        cur_path = full_path;

        let qid_type = if is_dir { 0x80 } else { 0x00 };
        wqids.push(Qid {
            ty: qid_type,
            version: 0,
            path: cur_qid,
        });
    }

    {
        let mut state = shared.state.lock().await;
        state.set_fid(new_fid, cur_qid, cur_path, is_dir);
    }

    let mut reply = Vec::new();
    reply.extend_from_slice(&(nwname as u16).to_le_bytes()); // nwqid
    for q in &wqids {
        encode_qid(&mut reply, *q);
    }
    send_reply(stream, RWALK, tag, &reply).await;
    Ok(())
}

async fn handle_open(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), mode(u8)
    if body.len() < 5 {
        return Err("short open message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _mode = body[4];

    let path = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).ok_or("unknown fid")?.path.clone()
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::Open,
        path,
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("open failed: {e}"))?;

    let mut state = shared.state.lock().await;
    if let Some(entry) = state.fids.get_mut(&fid) {
        entry.open = true;
    }

    let msize = *shared.msize.read().await;
    let mut reply = Vec::new();
    encode_qid(
        &mut reply,
        Qid {
            ty: 0,
            version: 0,
            path: 0,
        },
    );
    reply.extend_from_slice(&msize.to_le_bytes()); // iounit
    send_reply(stream, ROPEN, tag, &reply).await;
    Ok(())
}

async fn handle_read(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), offset(u64), count(u32)
    if body.len() < 16 {
        return Err("short read message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
    let _count = u32::from_le_bytes(body[12..16].try_into().unwrap());

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    if entry.is_dir {
        // ReadDir: send ReadDir to router
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::ReadDir,
            path: entry.path.clone(),
            data: Bytes::new(),
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let _resp = reply_rx
            .await
            .map_err(|_| "handler gone".to_string())?
            .map_err(|e| format!("readdir failed: {e}"))?;

        // Build directory listing from known paths
        let state = shared.state.lock().await;
        let prefix = if entry.path.ends_with('/') {
            entry.path.clone()
        } else {
            format!("{}/", entry.path)
        };

        let mut data = Vec::new();
        for (&qid, pent) in &state.paths {
            if pent.path == entry.path {
                continue;
            }
            if !pent.path.starts_with(&prefix) {
                continue;
                // Make sure it's a direct child (no nested /)
            }
            let rest = &pent.path[prefix.len()..];
            if rest.contains('/') {
                continue;
            }

            let name = if rest.is_empty() { "." } else { rest };
            let qid = Qid {
                ty: if pent.is_dir { 0x80 } else { 0x00 },
                version: 0,
                path: qid,
            };
            let length = 0;
            encode_stat(&mut data, name, qid, length, pent.is_dir);
        }

        // Check if we need to add a "." entry
        if offset == 0 {
            // Add "." for root
            let qid = Qid {
                ty: 0x80,
                version: 0,
                path: 0,
            };
            let mut dir_data = Vec::new();
            encode_stat(&mut dir_data, ".", qid, 0, true);
            dir_data.extend_from_slice(&data);
            data = dir_data;
        }

        let offset_us = offset as usize;
        let chunk: Vec<u8> = if offset_us < data.len() {
            data[offset_us..].to_vec()
        } else {
            Vec::new()
        };

        let mut reply = Vec::new();
        reply.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        reply.extend_from_slice(&chunk);
        send_reply(stream, RREAD, tag, &reply).await;
    } else {
        // Regular file read: send Read to router
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op: FsOperation::Read,
            path: entry.path.clone(),
            data: Bytes::new(),
            reply: reply_tx,
        };
        shared
            .router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let resp = reply_rx
            .await
            .map_err(|_| "handler gone".to_string())?
            .map_err(|e| format!("read failed: {e}"))?;

        let offset_us = offset as usize;
        let chunk: Vec<u8> = if offset_us < resp.data.len() {
            resp.data[offset_us..].to_vec()
        } else {
            Vec::new()
        };

        let mut reply = Vec::new();
        reply.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        reply.extend_from_slice(&chunk);
        send_reply(stream, RREAD, tag, &reply).await;
    }
    Ok(())
}

async fn handle_write(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    // Parse: fid(u32), offset(u64), count(u32), data[...]
    if body.len() < 16 {
        return Err("short write message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let _offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
    let count = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    let data = Bytes::from(body[16..16 + count.min(body.len() - 16)].to_vec());
    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::Write,
        path: entry.path,
        data,
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    let _resp = reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("write failed: {e}"))?;

    let mut reply = Vec::new();
    reply.extend_from_slice(&(count as u32).to_le_bytes());
    send_reply(stream, RWRITE, tag, &reply).await;
    Ok(())
}

async fn handle_clunk(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short clunk message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    {
        let state = shared.state.lock().await;
        let path = state.fids.get(&fid).map(|e| e.path.clone());
        if let Some(path) = path {
            // Send Release to router
            drop(state);
            let (reply_tx, reply_rx) = oneshot::channel();
            let req = Request {
                op: FsOperation::Release,
                path,
                data: Bytes::new(),
                reply: reply_tx,
            };
            if shared.router_tx.send(req).await.is_ok() {
                let _ = reply_rx.await;
            }
        }
    }

    let mut state = shared.state.lock().await;
    state.fids.remove(&fid);

    send_reply(stream, RCLUNK, tag, &[]).await;
    Ok(())
}

async fn handle_remove(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short remove message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    let path = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).ok_or("unknown fid")?.path.clone()
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::Unlink,
        path,
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("remove failed: {e}"))?;

    send_reply(stream, RREMOVE, tag, &[]).await;
    Ok(())
}

async fn handle_stat(
    shared: &Shared,
    stream: &mut impl NinepStream,
    tag: u16,
    body: &[u8],
) -> Result<(), String> {
    if body.len() < 4 {
        return Err("short stat message".into());
    }
    let fid = u32::from_le_bytes(body[0..4].try_into().unwrap());

    let entry = {
        let state = shared.state.lock().await;
        state.fids.get(&fid).cloned().ok_or("unknown fid")?
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let req = Request {
        op: FsOperation::GetAttr,
        path: entry.path.clone(),
        data: Bytes::new(),
        reply: reply_tx,
    };
    shared
        .router_tx
        .send(req)
        .await
        .map_err(|_| "router gone".to_string())?;
    let resp = reply_rx
        .await
        .map_err(|_| "handler gone".to_string())?
        .map_err(|e| format!("getattr failed: {e}"))?;

    let qid = Qid {
        ty: if entry.is_dir { 0x80 } else { 0x00 },
        version: 0,
        path: entry.qid,
    };

    let name = entry
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("/");

    let mut stat_buf = Vec::new();
    encode_stat(&mut stat_buf, name, qid, resp.data.len() as u64, entry.is_dir);

    let mut reply = Vec::new();
    reply.extend_from_slice(&(stat_buf.len() as u16).to_le_bytes());
    reply.extend_from_slice(&stat_buf);
    send_reply(stream, RSTAT, tag, &reply).await;
    Ok(())
}

async fn handle_flush(stream: &mut impl NinepStream, tag: u16) -> Result<(), String> {
    send_reply(stream, RFLUSH, tag, &[]).await;
    Ok(())
}

// ── TCP 9P2000 server ─────────────────────────────────────────────────

/// Start a 9P2000 server on the given TCP address.
///
/// Each incoming connection is handled in its own task with the same
/// protocol as the Unix socket variant.
pub async fn serve_tcp(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[9p-tcp] listening on {addr}");

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(8192),
    });

    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("[9p-tcp] connection from {peer}");
        let shared = shared.clone();
        tokio::spawn(async move {
            run_connection(stream, shared).await;
        });
    }
}

// ── UDP 9P2000 server ─────────────────────────────────────────────────

/// Start a 9P2000 server over UDP.
///
/// Each datagram is treated as a complete 9P message and the response is
/// sent back to the same peer address.  This is non-standard but useful
/// for lightweight / embedded scenarios.
pub async fn serve_udp(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
) -> std::io::Result<()> {
    let socket = tokio::net::UdpSocket::bind(addr).await?;
    eprintln!("[9p-udp] listening on {addr}");

    let shared = Arc::new(Shared {
        state: Mutex::new(NinepState::new()),
        router_tx,
        msize: RwLock::new(65536),
    });

    let mut buf = vec![0u8; 65536];
    loop {
        let (len, peer) = socket.recv_from(&mut buf).await?;
        let data = buf[..len].to_vec();
        let shared = shared.clone();

        // Process the message and send the reply back to the same peer.
        let reply = handle_udp_message(&shared, &data).await;
        if let Some(reply) = reply {
            let _ = socket.send_to(&reply, &peer).await;
        }
    }
}

/// Process a single 9P message for UDP (no persistent connection).
async fn handle_udp_message(
    shared: &Shared,
    data: &[u8],
) -> Option<Vec<u8>> {
    if data.len() < 7 {
        return None;
    }
    let msg_type = data[4];
    let tag = u16::from_le_bytes(data[5..7].try_into().unwrap());

    // Build a virtual stream wrapper to capture the reply bytes.
    let mut reply_buf = Vec::new();
    let mut virt = VirtualStream { buf: &mut reply_buf };

    let _ = match msg_type {
        TVERSION => handle_version(shared, &mut virt, tag, &data[7..]).await,
        TATTACH => handle_attach(shared, &mut virt, tag, &data[7..]).await,
        TWALK => handle_walk(shared, &mut virt, tag, &data[7..]).await,
        TOPEN => handle_open(shared, &mut virt, tag, &data[7..]).await,
        TREAD => handle_read(shared, &mut virt, tag, &data[7..]).await,
        TWRITE => handle_write(shared, &mut virt, tag, &data[7..]).await,
        TCLUNK => handle_clunk(shared, &mut virt, tag, &data[7..]).await,
        TREMOVE => handle_remove(shared, &mut virt, tag, &data[7..]).await,
        TSTAT => handle_stat(shared, &mut virt, tag, &data[7..]).await,
        TFLUSH => handle_flush(&mut virt, tag).await,
        _ => {
            // Send Rlerror for unknown message types.
            send_error(&mut virt, tag, &format!("unknown msg type {msg_type}")).await;
            Ok(())
        }
    };

    Some(reply_buf)
}

// ── Connection runner (shared between Unix and TCP) ───────────────────

async fn run_connection(stream: impl NinepStream, shared: Arc<Shared>) {
    let mut stream = stream;
    let buf = vec![0u8; 65536];
    loop {
        let mut header = [0u8; 7];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                eprintln!("[9p] read error: {e}");
                break;
            }
        }

        let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let msg_type = header[4];
        let tag = u16::from_le_bytes(header[5..7].try_into().unwrap());

        let body_len = size.saturating_sub(7);
        if body_len > buf.len() {
            eprintln!("[9p] message too large: {body_len}");
            break;
        }

        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            if let Err(e) = stream.read_exact(&mut body).await {
                eprintln!("[9p] read body error: {e}");
                break;
            }
        }

        if let Err(e) = handle_message(&shared, &mut stream, msg_type, tag, &body).await {
            eprintln!("[9p] handler error: {e}");
            send_error(&mut stream, tag, &e).await;
        }
    }
    let _ = stream.shutdown().await;
}

// ── Stream abstraction ────────────────────────────────────────────────

/// Trait abstracting over UnixStream and TcpStream for 9P message I/O.
trait NinepStream: Send {
    fn read_exact<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
    fn shutdown<'a>(&'a mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}

impl NinepStream for UnixStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        AsyncWriteExt::write_all(self, buf).await
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}

impl NinepStream for TcpStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        AsyncWriteExt::write_all(self, buf).await
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        AsyncWriteExt::shutdown(self).await
    }
}

/// Virtual stream that captures written bytes into a Vec (for UDP replies).
struct VirtualStream<'a> {
    buf: &'a mut Vec<u8>,
}

impl NinepStream for VirtualStream<'_> {
    async fn read_exact(&mut self, _buf: &mut [u8]) -> std::io::Result<()> {
        // Reads don't apply to virtual streams used for UDP replies.
        Ok(())
    }
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.buf.extend_from_slice(buf);
        Ok(())
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// We need RERROR message type for error replies
const RERROR: u8 = 107;
