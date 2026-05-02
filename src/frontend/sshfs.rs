//! SSH/SFTP frontend backed by the [`russh`] SSH-2.0 implementation and
//! [`russh_sftp`] protocol handler.
//!
//! Supports password and ed25519 public-key authentication.  Only the SFTP
//! subsystem is provided; each SFTP operation maps to a pinhead
//! `FsOperation` request forwarded through the path router.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::VerifyingKey;
use russh::server::{Auth, Config, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::fsop::FsOperation;
use crate::router::Request;

// ── SFTP handle manager ────────────────────────────────────────────────────

struct HandleState {
    next_id: u64,
    handles: HashMap<String, HandleEntry>,
}

#[derive(Clone)]
struct HandleEntry {
    path: String,
    is_dir: bool,
}

impl HandleState {
    fn new() -> Self {
        Self {
            next_id: 1,
            handles: HashMap::new(),
        }
    }

    fn alloc(&mut self, path: &str, is_dir: bool) -> String {
        let id = format!("{:016x}", self.next_id);
        self.next_id += 1;
        self.handles.insert(
            id.clone(),
            HandleEntry {
                path: path.to_string(),
                is_dir,
            },
        );
        id
    }

    fn get(&self, handle: &str) -> Option<&HandleEntry> {
        self.handles.get(handle)
    }

    fn free(&mut self, handle: &str) {
        self.handles.remove(handle);
    }
}

// ── Error types ────────────────────────────────────────────────────────────

/// Error type for the SSH-level handler.
#[derive(Debug)]
struct SshError(String);

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SshError {}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        SshError(e.to_string())
    }
}

impl From<String> for SshError {
    fn from(s: String) -> Self {
        SshError(s)
    }
}

// ── SSH Server (accepts connections) ───────────────────────────────────────

struct SshServer {
    router_tx: mpsc::Sender<Request>,
    password: Option<String>,
    authorized_keys: Vec<VerifyingKey>,
    userpasswds: Vec<(String, String)>,
    sem: Option<Arc<tokio::sync::Semaphore>>,
}

impl Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        // Non-blocking acquire.  If max_conns is reached, the connection
        // is dropped (SSH's new_client is synchronous, can't await).
        let permit = self.sem.as_ref()
            .and_then(|s| s.clone().try_acquire_owned().ok());
        SshSession {
            router_tx: self.router_tx.clone(),
            authorized_keys: self.authorized_keys.clone(),
            password: self.password.clone(),
            userpasswds: self.userpasswds.clone(),
            channels: HashMap::new(),
            _permit: permit,
        }
    }
}

// ── SSH Session (per-connection) ───────────────────────────────────────────

struct SshSession {
    router_tx: mpsc::Sender<Request>,
    authorized_keys: Vec<VerifyingKey>,
    password: Option<String>,
    userpasswds: Vec<(String, String)>,
    /// Channels awaiting subsystem requests, indexed by ChannelId.
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// Semaphore permit held for this connection's lifetime.
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Handler for SshSession {
    type Error = SshError;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = match &self.password {
            Some(expected) if password == expected => true,
            _ => self
                .userpasswds
                .iter()
                .any(|(u, p)| u == user && p == password),
        };
        let ok = ok || (self.password.is_none() && self.userpasswds.is_empty());

        if ok {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let trusted = self.key_is_trusted(public_key);
        if trusted || self.authorized_keys.is_empty() {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let trusted = self.key_is_trusted(public_key);
        if trusted || self.authorized_keys.is_empty() {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let id = channel.id();
        self.channels.insert(id, channel);
        Ok(true) // accept
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            session.channel_success(channel)?;

            if let Some(chan) = self.channels.remove(&channel) {
                let stream = chan.into_stream();
                let sftp_handler = SftpSession {
                    router_tx: self.router_tx.clone(),
                    handles: HandleState::new(),
                };
                tokio::spawn(async move {
                    sftp_loop(stream, sftp_handler).await;
                });
            }
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = session.close(channel);
        self.channels.remove(&channel);
        Ok(())
    }
}

impl SshSession {
    /// Check whether an `ssh_key::PublicKey` matches any of our trusted keys.
    fn key_is_trusted(&self, public_key: &russh::keys::PublicKey) -> bool {
        let ed = match public_key.key_data().ed25519() {
            Some(k) => k,
            None => return false,
        };
        let pub_bytes: &[u8; 32] = ed.as_ref();
        self.authorized_keys
            .iter()
            .any(|vk| vk.to_bytes().as_slice() == pub_bytes.as_slice())
    }
}

// ── SFTP Session (per-subsystem) ───────────────────────────────────────────

struct SftpSession {
    router_tx: mpsc::Sender<Request>,
    handles: HandleState,
}

impl SftpSession {
    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, StatusCode> {
        Ok(Version {
            version: 3,
            extensions: HashMap::new(),
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let creat = OpenFlags::CREATE;
        if (pflags.bits() & creat.bits()) != 0 {
            self.route(FsOperation::Create, &filename, Bytes::new())
                .await
                .map_err(|_| StatusCode::Failure)?;
        } else {
            self.route(FsOperation::Open, &filename, Bytes::new())
                .await
                .map_err(|_| StatusCode::Failure)?;
        }
        let handle = self.handles.alloc(&filename, false);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        if let Some(entry) = self.handles.get(&handle).cloned() {
            let _ = self
                .route(FsOperation::Release, &entry.path, Bytes::new())
                .await;
        }
        self.handles.free(&handle);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let entry = self
            .handles
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let resp = self
            .route(FsOperation::Read, &entry.path, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;
        let start = offset as usize;
        if start >= resp.len() {
            return Err(StatusCode::Eof);
        }
        let end = start.saturating_add(len as usize).min(resp.len());
        Ok(Data {
            id,
            data: resp[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        _offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let entry = self
            .handles
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        self.route(FsOperation::Write, &entry.path, Bytes::from(data))
            .await
            .map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        self.route(FsOperation::OpenDir, &path, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;
        let handle = self.handles.alloc(&path, true);
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        let entry = self
            .handles
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;

        if !entry.is_dir {
            return Err(StatusCode::Failure);
        }

        let resp = self
            .route(FsOperation::ReadDir, &entry.path, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;

        let text = String::from_utf8_lossy(&resp);
        let names: Vec<String> = text
            .split(|c: char| c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        let mut files = vec![File::dummy("."), File::dummy("..")];
        for name in names {
            files.push(File::dummy(name));
        }

        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusCode> {
        self.route(FsOperation::Unlink, &filename, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.route(FsOperation::MkDir, &path, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.route(FsOperation::RmDir, &path, Bytes::new())
            .await
            .map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let resolved = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        let resolved = resolved
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let resolved = format!("/{resolved}");

        Ok(Name {
            id,
            files: vec![File::dummy(resolved)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat_impl(id, &path).await
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat_impl(id, &path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        let entry = self
            .handles
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        self.stat_impl(id, &entry.path).await
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusCode> {
        let data = Bytes::from(newpath.clone());
        match self.route(FsOperation::Rename, &oldpath, data).await {
            Ok(_) => Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            }),
            Err(e) => Ok(Status {
                id,
                status_code: StatusCode::Failure,
                error_message: e,
                language_tag: "en".to_string(),
            }),
        }
    }

    /// Route a pinhead request and return the response data.
    async fn route(&self, op: FsOperation, path: &str, data: Bytes) -> Result<Bytes, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // Normalize: routes are registered with leading "/".
        let normalized = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let req = Request {
            op,
            path: normalized,
            data,
            reply: reply_tx,
        };
        self.router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let h_resp = timeout(Duration::from_secs(60), reply_rx)
            .await
            .map_err(|_| "handler timeout (60s)".to_string())?
            .map_err(|_| "handler gone".to_string())??;
        Ok(h_resp.data)
    }

    /// Shared implementation for stat/lstat/fstat.
    async fn stat_impl(&self, id: u32, path: &str) -> Result<Attrs, StatusCode> {
        let resp = self
            .route(FsOperation::GetAttr, path, Bytes::new())
            .await
            .map_err(|_| StatusCode::NoSuchFile)?;

        let resp_text = String::from_utf8_lossy(&resp).to_string();
        let is_dir = resp_text.contains("directory")
            || resp_text.contains("mode=directory")
            || resp_text.contains("dir");

        let size = if let Some(pos) = resp_text.find("size=") {
            let rest = &resp_text[pos + 5..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            num_str.parse::<u64>().unwrap_or(0)
        } else {
            resp.len() as u64
        };

        let mut attrs = FileAttributes::default();
        if size > 0 {
            attrs.size = Some(size);
        }
        let perms = if is_dir { 0o40755 } else { 0o100644 };
        attrs.permissions = Some(perms);

        Ok(Attrs { id, attrs })
    }
}

// ── Custom SFTP loop (replaces russh_sftp::server::run) ────────────────────

/// Read a single SFTP packet from the stream (4-byte big-endian length + payload).
async fn read_sftp_packet<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Result<Bytes, ()> {
    let length = stream.read_u32().await.map_err(|_| ())?;
    let mut buf = vec![0; length as usize];
    stream.read_exact(&mut buf).await.map_err(|_| ())?;
    Ok(Bytes::from(buf))
}

/// Build an SFTP Status packet for error responses.
fn make_status(id: u32, code: StatusCode) -> Packet {
    Packet::Status(Status {
        id,
        status_code: code,
        error_message: String::new(),
        language_tag: String::new(),
    })
}

/// Custom SFTP packet read/dispatch loop.
///
/// Handles the SFTP handshake (Init → Version) then loops on incoming
/// request packets, dispatching each to the corresponding `SftpSession`
/// method, serialising the response, and writing it back to the stream.
async fn sftp_loop<S>(mut stream: S, mut handler: SftpSession)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // ── Handshake ──────────────────────────────────────────────────────
    match read_sftp_packet(&mut stream).await {
        Ok(raw) => {
            if let Ok(Packet::Init(init)) = Packet::try_from(&mut Bytes::copy_from_slice(&raw)) {
                match handler.init(init.version, init.extensions).await {
                    Ok(version) => {
                        if let Ok(bytes) = Bytes::try_from(Packet::Version(version)) {
                            let _ = stream.write_all(&bytes).await;
                            let _ = stream.flush().await;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
        Err(_) => return,
    }

    // ── Main dispatch loop ─────────────────────────────────────────────
    loop {
        let raw = match read_sftp_packet(&mut stream).await {
            Ok(data) => data,
            Err(_) => break,
        };

        tokio::task::yield_now().await;

        let packet = match Packet::try_from(&mut Bytes::copy_from_slice(&raw)) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let response = match packet {
            Packet::Open(req) => {
                match handler.open(req.id, req.filename, req.pflags, req.attrs).await {
                    Ok(handle) => Packet::Handle(handle),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Close(req) => {
                match handler.close(req.id, req.handle).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Read(req) => {
                match handler.read(req.id, req.handle, req.offset, req.len).await {
                    Ok(data) => Packet::Data(data),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Write(req) => {
                match handler.write(req.id, req.handle, req.offset, req.data).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::OpenDir(req) => {
                match handler.opendir(req.id, req.path).await {
                    Ok(handle) => Packet::Handle(handle),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::ReadDir(req) => {
                match handler.readdir(req.id, req.handle).await {
                    Ok(name) => Packet::Name(name),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Remove(req) => {
                match handler.remove(req.id, req.filename).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::MkDir(req) => {
                match handler.mkdir(req.id, req.path, req.attrs).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::RmDir(req) => {
                match handler.rmdir(req.id, req.path).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::RealPath(req) => {
                match handler.realpath(req.id, req.path).await {
                    Ok(name) => Packet::Name(name),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Stat(req) => {
                match handler.stat(req.id, req.path).await {
                    Ok(attrs) => Packet::Attrs(attrs),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Lstat(req) => {
                match handler.lstat(req.id, req.path).await {
                    Ok(attrs) => Packet::Attrs(attrs),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Fstat(req) => {
                match handler.fstat(req.id, req.handle).await {
                    Ok(attrs) => Packet::Attrs(attrs),
                    Err(code) => make_status(req.id, code),
                }
            }

            Packet::Rename(req) => {
                match handler.rename(req.id, req.oldpath, req.newpath).await {
                    Ok(status) => Packet::Status(status),
                    Err(code) => make_status(req.id, code),
                }
            }

            // Unsupported operations → OpUnsupported
            Packet::SetStat(req) => {
                match handler
                    .route(FsOperation::SetAttr, &req.path, Bytes::new())
                    .await
                {
                    Ok(_) => Packet::Status(Status {
                        id: req.id,
                        status_code: StatusCode::Ok,
                        error_message: String::new(),
                        language_tag: String::new(),
                    }),
                    Err(_) => make_status(req.id, StatusCode::Failure),
                }
            }
            Packet::FSetStat(req) => {
                let path = handler
                    .handles
                    .get(&req.handle)
                    .map(|e| e.path.clone())
                    .unwrap_or_default();
                match handler.route(FsOperation::SetAttr, &path, Bytes::new()).await {
                    Ok(_) => Packet::Status(Status {
                        id: req.id,
                        status_code: StatusCode::Ok,
                        error_message: String::new(),
                        language_tag: String::new(),
                    }),
                    Err(_) => make_status(req.id, StatusCode::Failure),
                }
            }
            Packet::ReadLink(req) => make_status(req.id, StatusCode::OpUnsupported),
            Packet::Symlink(req) => make_status(req.id, StatusCode::OpUnsupported),

            // Responses and unknown → silently skip
            _ => continue,
        };

        if let Ok(bytes) = Bytes::try_from(response) {
            let _ = stream.write_all(&bytes).await;
            let _ = stream.flush().await;
        }
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Configuration for the SSHFS frontend.
pub struct SshfsConfig {
    /// Global password for password authentication (`None` = no global password).
    pub password: Option<String>,
    /// Path to an `authorized_keys` file (one ed25519 public key per line).
    pub authorized_keys_path: Option<String>,
    /// Username/password pairs for per-user authentication.
    pub userpasswds: Vec<(String, String)>,
    /// Max concurrent connections (None = unlimited). Queues beyond limit.
    pub max_conns: Option<usize>,
}

/// Start an SSH/SFTP server on the given TCP address using `russh`.
///
/// Each incoming connection is handled by a `russh`-managed task; SFTP
/// subsystem requests are dispatched to the pinhead path router.
pub async fn serve(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
    config: SshfsConfig,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;

    // Generate an ed25519 host key (rng is scoped to avoid !Send across await).
    let host_key = {
        let mut rng = russh::keys::key::safe_rng();
        russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).unwrap()
    };

    // Load authorized keys.
    let authorized_keys = if let Some(path) = &config.authorized_keys_path {
        load_authorized_keys(path).unwrap_or_default()
    } else {
        Vec::new()
    };

    let russh_config = Arc::new(Config {
        auth_rejection_time: Duration::from_secs(3),
        keys: vec![host_key],
        ..Default::default()
    });

    let mut server = SshServer {
        router_tx,
        password: config.password,
        authorized_keys,
        userpasswds: config.userpasswds,
        sem: config.max_conns.map(|n| Arc::new(tokio::sync::Semaphore::new(n))),
    };

    server.run_on_socket(russh_config, &listener).await
}

/// Load ed25519 public keys from an `authorized_keys` file.
fn load_authorized_keys(path: &str) -> Result<Vec<VerifyingKey>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut keys = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse "ssh-ed25519 <base64> [comment]"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 || parts[0] != "ssh-ed25519" {
            continue;
        }

        if let Ok(key_bytes) = simple_base64_decode(parts[1])
            && key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key_bytes);
                if let Ok(vk) = VerifyingKey::from_bytes(&arr) {
                    keys.push(vk);
                }
            }
    }

    Ok(keys)
}

/// Minimal base64 decode (no padding needed for 32-byte ed25519 keys).
fn simple_base64_decode(input: &str) -> Result<Vec<u8>, String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;

    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        let val = CHARS
            .iter()
            .position(|&x| x == c)
            .ok_or("invalid base64")? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            result.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(result)
}
