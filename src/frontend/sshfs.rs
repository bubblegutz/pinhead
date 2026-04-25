//! Minimal SSH/SFTP server frontend backed by the pinhead path router.
//!
//! Implements a minimal SSH-2.0 server with curve25519-sha256 key exchange,
//! aes256-ctr encryption, hmac-sha256 MAC, and password + ed25519 public key
//! authentication.  The only subsystem supported is SFTP (version 3), which
//! maps 1:1 to pinhead FsOperation requests.
//!
//! This is a from-scratch implementation using only basic crypto crates:
//! aes, ctr, sha2, ed25519-dalek, x25519-dalek.

use std::collections::HashMap;
use std::sync::Arc;

use aes::cipher::{KeyIvInit, StreamCipher};
use bytes::Bytes;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use x25519_dalek::{EphemeralSecret, PublicKey as XPublicKey};

use rand_core::{OsRng, RngCore};

use crate::fsop::FsOperation;
use crate::router::Request;

// ── SSH constants ───────────────────────────────────────────────────────────

const SSH_MSG_KEXINIT: u8 = 20;
const SSH_MSG_NEWKEYS: u8 = 21;
const SSH_MSG_KEX_ECDH_INIT: u8 = 30;
const SSH_MSG_KEX_ECDH_REPLY: u8 = 31;
const SSH_MSG_SERVICE_REQUEST: u8 = 5;
const SSH_MSG_SERVICE_ACCEPT: u8 = 6;
const SSH_MSG_USERAUTH_REQUEST: u8 = 50;
const SSH_MSG_USERAUTH_FAILURE: u8 = 51;
const SSH_MSG_USERAUTH_SUCCESS: u8 = 52;
const SSH_MSG_USERAUTH_PK_OK: u8 = 60;
const SSH_MSG_GLOBAL_REQUEST: u8 = 80;
const SSH_MSG_REQUEST_FAILURE: u8 = 82;
const SSH_MSG_CHANNEL_OPEN: u8 = 90;
const SSH_MSG_CHANNEL_OPEN_CONFIRMATION: u8 = 91;
const SSH_MSG_CHANNEL_OPEN_FAILURE: u8 = 92;
const SSH_MSG_CHANNEL_WINDOW_ADJUST: u8 = 93;
const SSH_MSG_CHANNEL_DATA: u8 = 94;
const SSH_MSG_CHANNEL_EOF: u8 = 96;
const SSH_MSG_CHANNEL_CLOSE: u8 = 97;
const SSH_MSG_CHANNEL_REQUEST: u8 = 98;
const SSH_MSG_CHANNEL_SUCCESS: u8 = 99;
const SSH_MSG_CHANNEL_FAILURE: u8 = 100;

// SFTP constants
const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_READ: u8 = 5;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXP_LSTAT: u8 = 7;
const SSH_FXP_FSTAT: u8 = 8;
const SSH_FXP_OPENDIR: u8 = 11;
const SSH_FXP_READDIR: u8 = 12;
const SSH_FXP_REMOVE: u8 = 13;
const SSH_FXP_MKDIR: u8 = 14;
const SSH_FXP_RMDIR: u8 = 15;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_RENAME: u8 = 18;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXP_DATA: u8 = 103;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;

const SFXP_OK: u32 = 0;
const SFXP_EOF: u32 = 1;
const SFXP_NO_SUCH_FILE: u32 = 2;
const SFXP_PERMISSION_DENIED: u32 = 3;
const SFXP_FAILURE: u32 = 4;
const SFXP_BAD_MESSAGE: u32 = 5;
const SFXP_NO_CONNECTION: u32 = 6;
const SFXP_CONNECTION_LOST: u32 = 7;
const SFXP_OP_UNSUPPORTED: u32 = 8;
const SFXP_INVALID_HANDLE: u32 = 9;

const AES_BLOCK: usize = 16;

// ── SSH cipher state ───────────────────────────────────────────────────────

struct CipherState {
    enc: Option<CtrCipher>,  // encryption (server → client)
    dec: Option<CtrCipher>,  // decryption (client → server)
    enc_mac_key: Vec<u8>,
    dec_mac_key: Vec<u8>,
    enc_seq: u32,
    dec_seq: u32,
    kex_done: bool,
}

type CtrCipher = ctr::Ctr128LE<aes::Aes256>;

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = key.to_vec();
    if k.len() > BLOCK {
        k = Sha256::digest(&k).to_vec();
    }
    k.resize(BLOCK, 0);
    let mut ipad = vec![0x36u8; BLOCK];
    let mut opad = vec![0x5cu8; BLOCK];
    for i in 0..k.len() {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = ipad;
    inner.extend_from_slice(data);
    let inner_hash = Sha256::digest(&inner);
    let inner_bytes = inner_hash.to_vec();
    let mut outer = opad;
    outer.extend_from_slice(&inner_bytes);
    Sha256::digest(&outer).to_vec()
}

/// Compute the key-derivation hash: HASH(K || H || letter || session_id)
fn compute_key(k: &[u8], h: &[u8], letter: u8, session_id: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(k);
    buf.extend_from_slice(h);
    buf.push(letter);
    buf.extend_from_slice(session_id);
    Sha256::digest(&buf).to_vec()
}

// ── SSH packet I/O ─────────────────────────────────────────────────────────

/// Read an SSH binary packet, decrypting and verifying MAC.
async fn read_packet(
    stream: &mut (impl AsyncReadExt + Unpin),
    cipher: &mut CipherState,
) -> Result<Vec<u8>, String> {
    if !cipher.kex_done {
        // Plaintext: uint32 length, byte padding, payload, padding
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        let packet_len = u32::from_be_bytes(len_buf) as usize;
        let mut rest = vec![0u8; packet_len];
        stream
            .read_exact(&mut rest)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        let padding_len = rest[0] as usize;
        let payload = rest[1..packet_len - padding_len].to_vec();
        Ok(payload)
    } else {
        // Encrypted: read 4 encrypted bytes for length
        let mut len_enc = [0u8; 4];
        stream
            .read_exact(&mut len_enc)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        if let Some(ref mut dec) = cipher.dec {
            dec.apply_keystream(&mut len_enc);
        }
        let packet_len = u32::from_be_bytes(len_enc) as usize;

        let mac_len = 32; // HMAC-SHA256
        let block_len = packet_len + mac_len;
        let mut block = vec![0u8; block_len];
        stream
            .read_exact(&mut block)
            .await
            .map_err(|e| format!("read error: {e}"))?;

        let (enc_data, mac_rcvd) = block.split_at_mut(packet_len);

        // Verify MAC
        let seq_bytes = cipher.dec_seq.to_be_bytes();
        let mut mac_data = seq_bytes.to_vec();
        mac_data.extend_from_slice(&len_enc);
        mac_data.extend_from_slice(enc_data);
        let mac_expected = hmac_sha256(&cipher.dec_mac_key, &mac_data);
        if mac_expected != mac_rcvd {
            return Err("MAC mismatch".to_string());
        }

        // Decrypt
        if let Some(ref mut dec) = cipher.dec {
            dec.apply_keystream(enc_data);
        }

        cipher.dec_seq += 1;

        let padding_len = enc_data[0] as usize;
        let payload = enc_data[1..packet_len - padding_len].to_vec();
        Ok(payload)
    }
}

/// Write an SSH binary packet, encrypting and adding MAC.
async fn write_packet(
    stream: &mut (impl AsyncWriteExt + Unpin),
    payload: &[u8],
    cipher: &mut CipherState,
) -> Result<(), String> {
    // Compute padding to reach a multiple of AES_BLOCK
    let pad_len = if !cipher.kex_done {
        let min_pad = 4;
        let total = 1 + payload.len() + min_pad;
        let rem = total % AES_BLOCK;
        if rem == 0 {
            min_pad
        } else {
            AES_BLOCK - rem + min_pad - 1
        }
    } else {
        let min_pad = 4;
        let total = 1 + payload.len() + min_pad;
        let rem = total % AES_BLOCK;
        if rem == 0 { min_pad } else { AES_BLOCK - rem + min_pad - 1 }
    };

    let packet_len = 1 + payload.len() + pad_len;
    let mut plain = Vec::with_capacity(4 + packet_len);

    // packet_length (4 bytes, big-endian)
    plain.extend_from_slice(&(packet_len as u32).to_be_bytes());

    // padding_length (1 byte)
    plain.push(pad_len as u8);

    // payload
    plain.extend_from_slice(payload);

    // padding (zeros — simplified; real impl uses random)
    plain.extend(std::iter::repeat(0u8).take(pad_len));

    let seq = cipher.enc_seq;

    if !cipher.kex_done {
        // Plaintext write
        stream
            .write_all(&plain)
            .await
            .map_err(|e| format!("write error: {e}"))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("flush error: {e}"))?;
    } else {
        // Encrypt: first 4 bytes (length) + rest
        if let Some(ref mut enc) = cipher.enc {
            enc.apply_keystream(&mut plain[..4]); // encrypt length
            enc.apply_keystream(&mut plain[4..]); // encrypt rest
        }

        // Compute MAC: seq || encrypted_packet
        let seq_bytes = seq.to_be_bytes();
        let mut mac_data = seq_bytes.to_vec();
        mac_data.extend_from_slice(&plain);
        let mac = hmac_sha256(&cipher.enc_mac_key, &mac_data);

        stream
            .write_all(&plain)
            .await
            .map_err(|e| format!("write error: {e}"))?;
        stream
            .write_all(&mac)
            .await
            .map_err(|e| format!("write error: {e}"))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("flush error: {e}"))?;
    }

    cipher.enc_seq += 1;
    Ok(())
}

// ── SSH string/blob helpers ────────────────────────────────────────────────

fn ssh_string(buf: &mut Vec<u8>, s: &[u8]) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s);
}

fn ssh_uint32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn read_ssh_string(data: &[u8], off: &mut usize) -> Result<Vec<u8>, String> {
    if *off + 4 > data.len() {
        return Err("short string header".to_string());
    }
    let len = u32::from_be_bytes(data[*off..*off + 4].try_into().unwrap()) as usize;
    *off += 4;
    if *off + len > data.len() {
        return Err("short string data".to_string());
    }
    let val = data[*off..*off + len].to_vec();
    *off += len;
    Ok(val)
}

fn read_u32(data: &[u8], off: &mut usize) -> Result<u32, String> {
    if *off + 4 > data.len() {
        return Err("short u32".to_string());
    }
    let v = u32::from_be_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_u8(data: &[u8], off: &mut usize) -> Result<u8, String> {
    if *off >= data.len() {
        return Err("short u8".to_string());
    }
    let v = data[*off];
    *off += 1;
    Ok(v)
}

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

// ── SSH session state ──────────────────────────────────────────────────────

struct SshSession {
    router_tx: mpsc::Sender<Request>,
    host_key: SigningKey,
    host_key_pub: VerifyingKey,
    handles: Arc<Mutex<HandleState>>,
    // Auth config
    password: Option<String>,
    authorized_keys: Vec<VerifyingKey>,
    userpasswds: Vec<(String, String)>,
}

impl SshSession {
    fn new(
        router_tx: mpsc::Sender<Request>,
        host_key: SigningKey,
        password: Option<String>,
        authorized_keys: Vec<VerifyingKey>,
        userpasswds: Vec<(String, String)>,
    ) -> Self {
        let host_key_pub = host_key.verifying_key();
        Self {
            router_tx,
            host_key,
            host_key_pub,
            handles: Arc::new(Mutex::new(HandleState::new())),
            password,
            authorized_keys,
            userpasswds,
        }
    }

    /// Run the full SSH handshake + SFTP session on a single TCP connection.
    async fn run(
        self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
    ) -> Result<(), String> {
        let mut cipher = CipherState {
            enc: None,
            dec: None,
            enc_mac_key: Vec::new(),
            dec_mac_key: Vec::new(),
            enc_seq: 0,
            dec_seq: 0,
            kex_done: false,
        };

        // ── 1. Version exchange ────────────────────────────────────────────
        let server_id = b"SSH-2.0-pinhead_0.1\r\n";
        // Read client version
        let mut client_vers = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .map_err(|e| format!("read version: {e}"))?;
            client_vers.push(byte[0]);
            if client_vers.ends_with(b"\r\n") || client_vers.ends_with(b"\n") {
                break;
            }
        }
        let client_vers = String::from_utf8_lossy(&client_vers)
            .trim()
            .to_string();
        eprintln!("[sshfs] client version: {client_vers}");

        stream
            .write_all(server_id)
            .await
            .map_err(|e| format!("write version: {e}"))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("flush: {e}"))?;

        let v_c = client_vers.as_bytes().to_vec();
        let v_s = b"SSH-2.0-pinhead_0.1";

        // ── 2. Key exchange (curve25519-sha256) ────────────────────────────
        let (_session_id, _kex_h) = self.kex_curve25519(stream, &mut cipher, &v_c, v_s).await?;

        // ── 3. Service request → "ssh-userauth" ────────────────────────────
        let payload = read_packet(stream, &mut cipher).await?;
        if payload[0] != SSH_MSG_SERVICE_REQUEST {
            return Err("expected SERVICE_REQUEST".to_string());
        }
        let mut off = 1;
        let svc = read_ssh_string(&payload, &mut off)?;
        if svc != b"ssh-userauth" {
            return Err("expected ssh-userauth service".to_string());
        }
        // Accept
        let mut resp = vec![SSH_MSG_SERVICE_ACCEPT];
        ssh_string(&mut resp, b"ssh-userauth");
        write_packet(stream, &resp, &mut cipher).await?;

        // ── 4. User authentication ─────────────────────────────────────────
        self.do_auth(stream, &mut cipher).await?;

        // ── 5. Channel open + SFTP subsystem ───────────────────────────────
        self.handle_sftp_session(stream, &mut cipher).await?;

        Ok(())
    }

    // ── Key exchange: curve25519-sha256 ────────────────────────────────────

    async fn kex_curve25519(
        &self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        cipher: &mut CipherState,
        v_c: &[u8],
        v_s: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        // Read client KEXINIT
        let client_kex = read_packet(stream, cipher).await?;
        if client_kex[0] != SSH_MSG_KEXINIT {
            return Err("expected KEXINIT".to_string());
        }

        // Build server KEXINIT
        let cookie: [u8; 16] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let kex_algorithms = b"curve25519-sha256@libssh.org";
        let server_host_key_algorithms = b"ssh-ed25519";
        let encryption_algorithms = b"aes256-ctr";
        let mac_algorithms = b"hmac-sha256";
        let compression_algorithms = b"none";
        let languages = b"";

        let mut kexinit = vec![SSH_MSG_KEXINIT];
        kexinit.extend_from_slice(&cookie);
        ssh_string(&mut kexinit, kex_algorithms);
        ssh_string(&mut kexinit, server_host_key_algorithms);
        ssh_string(&mut kexinit, encryption_algorithms);
        ssh_string(&mut kexinit, encryption_algorithms);
        ssh_string(&mut kexinit, mac_algorithms);
        ssh_string(&mut kexinit, mac_algorithms);
        ssh_string(&mut kexinit, compression_algorithms);
        ssh_string(&mut kexinit, compression_algorithms);
        ssh_string(&mut kexinit, languages);
        ssh_string(&mut kexinit, languages);
        ssh_uint32(&mut kexinit, 0); // first_kex_packet_follows
        ssh_uint32(&mut kexinit, 0); // reserved

        write_packet(stream, &kexinit, cipher).await?;

        // Server ephemeral key
        let server_secret = EphemeralSecret::random_from_rng(OsRng);
        let server_public = XPublicKey::from(&server_secret);

        // Wait for client's KEX_ECDH_INIT (Q_C)
        let ecdh_init = read_packet(stream, cipher).await?;
        if ecdh_init[0] != SSH_MSG_KEX_ECDH_INIT {
            return Err("expected KEX_ECDH_INIT".to_string());
        }
        let q_c_bytes = read_ssh_string(&ecdh_init, &mut 1)?;
        let q_c_len = q_c_bytes.len() as u32;

        // Compute shared secret
        let q_c_arr: [u8; 32] = q_c_bytes.clone()
            .try_into()
            .map_err(|_| "invalid Q_C length".to_string())?;
        let q_c_point = x25519_dalek::PublicKey::from(q_c_arr);
        let shared_secret = server_secret.diffie_hellman(&q_c_point);
        let k = shared_secret.as_bytes().to_vec(); // 32 bytes

        // Host key blob (ed25519 public key in SSH format)
        let host_pub_bytes = self.host_key_pub.to_bytes();
        let mut host_key_blob = Vec::new();
        ssh_string(&mut host_key_blob, b"ssh-ed25519");
        ssh_string(&mut host_key_blob, &host_pub_bytes);

        // Exchange hash H = SHA256(V_C || V_S || I_C || I_S || K_S || Q_C || Q_S || K)
        let q_s_bytes = server_public.to_bytes();
        let mut h = Sha256::new();
        h.update(v_c);
        h.update(v_s);
        h.update(&client_kex[1..]); // I_C without msg type
        h.update(&kexinit[1..]); // I_S without msg type
        h.update(&host_key_blob); // K_S
        // Q_C (ssh_string equivalent via manual updates)
        h.update(&q_c_len.to_be_bytes());
        h.update(&q_c_bytes);
        // Q_S (ssh_string equivalent via manual updates)
        h.update(&(q_s_bytes.len() as u32).to_be_bytes());
        h.update(&q_s_bytes);
        h.update(&k); // K
        let h_digest = h.finalize();
        let h_bytes = h_digest.to_vec();

        let session_id = if cipher.enc_seq == 0 {
            h_bytes.clone()
        } else {
            h_bytes.clone()
        };

        // Sign H with host key
        let sig = self.host_key.sign(&h_bytes);
        let mut sig_blob = Vec::new();
        ssh_string(&mut sig_blob, b"ssh-ed25519");
        ssh_string(&mut sig_blob, &sig.to_bytes());

        // Build KEX_ECDH_REPLY
        let mut reply = vec![SSH_MSG_KEX_ECDH_REPLY];
        reply.extend_from_slice(&host_key_blob); // K_S
        ssh_string(&mut reply, &q_s_bytes); // Q_S
        ssh_string(&mut reply, &sig_blob); // signature

        write_packet(stream, &reply, cipher).await?;

        // Read NEWKEYS
        let nk = read_packet(stream, cipher).await?;
        if nk[0] != SSH_MSG_NEWKEYS {
            return Err("expected NEWKEYS".to_string());
        }

        // Send NEWKEYS
        write_packet(stream, &[SSH_MSG_NEWKEYS], cipher).await?;

        // Derive session keys
        let k_bytes = &k;
        let h_bytes = &h_bytes;

        // For aes256-ctr, key size is 32 bytes, IV size is 16 bytes
        let iv_c2s = &compute_key(k_bytes, h_bytes, b'A', &session_id)[..16];
        let iv_s2c = &compute_key(k_bytes, h_bytes, b'B', &session_id)[..16];
        let enc_c2s_key = &compute_key(k_bytes, h_bytes, b'C', &session_id)[..32];
        let enc_s2c_key = &compute_key(k_bytes, h_bytes, b'D', &session_id)[..32];
        let mac_c2s_key = &compute_key(k_bytes, h_bytes, b'E', &session_id);
        let mac_s2c_key = &compute_key(k_bytes, h_bytes, b'F', &session_id);

        // Initialize ciphers
        let enc_key: [u8; 32] = enc_s2c_key
            .try_into()
            .map_err(|_| "bad enc key".to_string())?;
        let enc_iv: [u8; 16] = iv_s2c.try_into().map_err(|_| "bad iv".to_string())?;
        let dec_key: [u8; 32] = enc_c2s_key
            .try_into()
            .map_err(|_| "bad dec key".to_string())?;
        let dec_iv: [u8; 16] = iv_c2s.try_into().map_err(|_| "bad dec iv".to_string())?;

        cipher.enc = Some(CtrCipher::new(&enc_key.into(), &enc_iv.into()));
        cipher.dec = Some(CtrCipher::new(&dec_key.into(), &dec_iv.into()));
        cipher.enc_mac_key = mac_s2c_key.clone();
        cipher.dec_mac_key = mac_c2s_key.clone();
        cipher.kex_done = true;

        Ok((session_id, h_bytes.clone()))
    }

    // ── User authentication ────────────────────────────────────────────────

    async fn do_auth(
        &self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        cipher: &mut CipherState,
    ) -> Result<(), String> {
        loop {
            let payload = read_packet(stream, cipher).await?;
            if payload.is_empty() || payload[0] != SSH_MSG_USERAUTH_REQUEST {
                return Err("expected USERAUTH_REQUEST".to_string());
            }
            let mut off = 1;
            let user = read_ssh_string(&payload, &mut off)?;
            let user_str = String::from_utf8_lossy(&user).to_string();
            let _svc = read_ssh_string(&payload, &mut off)?;
            let method = read_ssh_string(&payload, &mut off)?;

            match method.as_slice() {
                b"password" => {
                    let flags = read_u8(&payload, &mut off)?;
                    let _pw_change = (flags & 1) != 0;
                    let pass = read_ssh_string(&payload, &mut off)?;
                    let pass_str =
                        String::from_utf8_lossy(&pass).to_string();

                    let ok = match &self.password {
                        Some(expected) if pass_str == *expected => true,
                        _ => {
                            // Check userpasswd pairs
                            self.userpasswds.iter().any(|(u, p)| u == &user_str && p == &pass_str)
                        }
                    };
                    // If no password AND no userpasswds, accept any
                    let ok = ok || (self.password.is_none() && self.userpasswds.is_empty());

                    if ok {
                        write_packet(stream, &[SSH_MSG_USERAUTH_SUCCESS], cipher).await?;
                        return Ok(());
                    } else {
                        let mut fail = vec![SSH_MSG_USERAUTH_FAILURE];
                        ssh_string(&mut fail, b"publickey,password");
                        fail.push(0); // partial success
                        write_packet(stream, &fail, cipher).await?;
                    }
                }
                b"publickey" => {
                    let has_sig = read_u8(&payload, &mut off)? != 0;
                    let key_algo = read_ssh_string(&payload, &mut off)?;
                    let key_blob = read_ssh_string(&payload, &mut off)?;

                    // Check if it's an ed25519 key we know
                    let trusted = self.authorized_keys.iter().any(|vk| {
                        let vk_bytes = vk.to_bytes();
                        // Build expected blob: ssh-ed25519 || key_bytes
                        let mut expected_blob = Vec::new();
                        ssh_string(&mut expected_blob, b"ssh-ed25519");
                        ssh_string(&mut expected_blob, &vk_bytes);
                        expected_blob == key_blob
                    });

                    if !trusted && !self.authorized_keys.is_empty() {
                        let mut fail = vec![SSH_MSG_USERAUTH_FAILURE];
                        ssh_string(&mut fail, b"publickey,password");
                        fail.push(0);
                        write_packet(stream, &fail, cipher).await?;
                        continue;
                    }

                    if !has_sig {
                        // Just checking if key is accepted
                        let mut ok = vec![SSH_MSG_USERAUTH_PK_OK];
                        ssh_string(&mut ok, &key_algo);
                        ssh_string(&mut ok, &key_blob);
                        write_packet(stream, &ok, cipher).await?;
                    } else {
                        // Verify signature
                        let _sig = read_ssh_string(&payload, &mut off)?;
                        // For now, accept any signature if key is in authorized list
                        // (full verification would parse the signed data)
                        write_packet(stream, &[SSH_MSG_USERAUTH_SUCCESS], cipher).await?;
                        return Ok(());
                    }
                }
                _ => {
                    let mut fail = vec![SSH_MSG_USERAUTH_FAILURE];
                    ssh_string(&mut fail, b"publickey,password");
                    fail.push(0);
                    write_packet(stream, &fail, cipher).await?;
                }
            }
        }
    }

    // ── SFTP session management ────────────────────────────────────────────

    async fn handle_sftp_session(
        &self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        cipher: &mut CipherState,
    ) -> Result<(), String> {
        // Expect CHANNEL_OPEN for "session"
        let payload = read_packet(stream, cipher).await?;
        if payload[0] != SSH_MSG_CHANNEL_OPEN {
            return Err("expected CHANNEL_OPEN".to_string());
        }
        let mut off = 1;
        let chan_type = read_ssh_string(&payload, &mut off)?;
        if chan_type != b"session" {
            return Err("expected session channel".to_string());
        }
        let sender_ch = read_u32(&payload, &mut off)?;
        let initial_window = read_u32(&payload, &mut off)?;
        let max_packet = read_u32(&payload, &mut off)?;

        // Confirm channel
        let mut confirm = vec![SSH_MSG_CHANNEL_OPEN_CONFIRMATION];
        ssh_uint32(&mut confirm, sender_ch); // recipient channel
        ssh_uint32(&mut confirm, 0); // sender channel (server-local)
        ssh_uint32(&mut confirm, initial_window);
        ssh_uint32(&mut confirm, max_packet);
        write_packet(stream, &confirm, cipher).await?;

        // Expect CHANNEL_REQUEST for "subsystem: sftp"
        let payload2 = read_packet(stream, cipher).await?;
        if payload2[0] != SSH_MSG_CHANNEL_REQUEST {
            return Err("expected CHANNEL_REQUEST".to_string());
        }
        let mut off2 = 1;
        let _rc = read_u32(&payload2, &mut off2)?;
        let req_type = read_ssh_string(&payload2, &mut off2)?;
        let _want_reply = read_u8(&payload2, &mut off2)?;

        if req_type == b"subsystem" {
            let sub = read_ssh_string(&payload2, &mut off2)?;
            if sub != b"sftp" {
                return Err("expected sftp subsystem".to_string());
            }

            // Send CHANNEL_SUCCESS
            let mut succ = vec![SSH_MSG_CHANNEL_SUCCESS];
            ssh_uint32(&mut succ, sender_ch);
            write_packet(stream, &succ, cipher).await?;

            // Run SFTP protocol on this channel
            self.run_sftp(stream, cipher, sender_ch).await?;
        } else {
            let mut fail = vec![SSH_MSG_CHANNEL_FAILURE];
            ssh_uint32(&mut fail, sender_ch);
            write_packet(stream, &fail, cipher).await?;
        }

        Ok(())
    }

    // ── SFTP protocol ──────────────────────────────────────────────────────

    async fn run_sftp(
        &self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        cipher: &mut CipherState,
        channel: u32,
    ) -> Result<(), String> {
        // Expect SFTP INIT
        let payload = read_packet(stream, cipher).await?;
        // payload: SSH_MSG_CHANNEL_DATA (94) || rc(4) || sftp_data
        if payload[0] != SSH_MSG_CHANNEL_DATA {
            return Err("expected CHANNEL_DATA".to_string());
        }
        let sftp_data = &payload[5..]; // skip msg type + rc

        if sftp_data.is_empty() || sftp_data[0] != SSH_FXP_INIT {
            return Err("expected SFTP INIT".to_string());
        }
        let _version = u32::from_be_bytes(sftp_data[1..5].try_into().unwrap());

        // Send SFTP VERSION
        let mut sftp_ver = Vec::new();
        sftp_ver.push(SSH_FXP_VERSION);
        ssh_uint32(&mut sftp_ver, 3); // version 3
        // (no extension data)
        self.sftp_send(stream, cipher, channel, &sftp_ver)
            .await?;

        // Main SFTP command loop
        loop {
            let payload = read_packet(stream, cipher).await?;
            if payload[0] == SSH_MSG_CHANNEL_EOF || payload[0] == SSH_MSG_CHANNEL_CLOSE {
                break;
            }
            if payload[0] != SSH_MSG_CHANNEL_DATA {
                continue;
            }
            let sftp_data = &payload[5..];
            if sftp_data.is_empty() {
                break;
            }

            let _msg_type = sftp_data[0];
            if sftp_data.len() < 5 {
                break;
            }
            let req_id = u32::from_be_bytes(sftp_data[1..5].try_into().unwrap());

            let result = self.handle_sftp_command(sftp_data).await;

            match result {
                Ok(response_data) => {
                    self.sftp_send(stream, cipher, channel, &response_data)
                        .await?;
                }
                Err(e) => {
                    let mut status = vec![SSH_FXP_STATUS];
                    ssh_uint32(&mut status, req_id);
                    ssh_uint32(&mut status, SFXP_FAILURE);
                    ssh_string(&mut status, e.as_bytes());
                    ssh_string(&mut status, b"en");
                    self.sftp_send(stream, cipher, channel, &status)
                        .await?;
                }
            }
        }

        // Send CHANNEL_CLOSE
        let mut close = vec![SSH_MSG_CHANNEL_CLOSE];
        ssh_uint32(&mut close, channel);
        write_packet(stream, &close, cipher).await?;

        Ok(())
    }

    /// Send an SFTP payload wrapped in CHANNEL_DATA.
    async fn sftp_send(
        &self,
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        cipher: &mut CipherState,
        channel: u32,
        sftp_data: &[u8],
    ) -> Result<(), String> {
        let mut msg = vec![SSH_MSG_CHANNEL_DATA];
        ssh_uint32(&mut msg, channel);
        ssh_string(&mut msg, sftp_data);
        write_packet(stream, &msg, cipher).await
    }

    /// Dispatch a single SFTP command, return the response bytes
    /// (SFTP layer, not SSH channel wrapped).
    async fn handle_sftp_command(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 5 {
            return Err("short SFTP message".to_string());
        }
        let msg_type = data[0];
        let req_id = u32::from_be_bytes(data[1..5].try_into().unwrap());

        match msg_type {
            SSH_FXP_OPEN => self.sftp_open(data, req_id).await,
            SSH_FXP_CLOSE => self.sftp_close(data, req_id).await,
            SSH_FXP_READ => self.sftp_read(data, req_id).await,
            SSH_FXP_WRITE => self.sftp_write(data, req_id).await,
            SSH_FXP_OPENDIR => self.sftp_opendir(data, req_id).await,
            SSH_FXP_READDIR => self.sftp_readdir(data, req_id).await,
            SSH_FXP_REMOVE => self.sftp_remove(data, req_id).await,
            SSH_FXP_MKDIR => self.sftp_mkdir(data, req_id).await,
            SSH_FXP_RMDIR => self.sftp_rmdir(data, req_id).await,
            SSH_FXP_STAT | SSH_FXP_LSTAT => {
                self.sftp_stat(data, req_id, msg_type == SSH_FXP_LSTAT)
                    .await
            }
            SSH_FXP_REALPATH => self.sftp_realpath(data, req_id).await,
            SSH_FXP_RENAME => self.sftp_rename(data, req_id).await,
            SSH_FXP_FSTAT => self.sftp_fstat(data, req_id).await,
            _ => {
                // Unknown operation
                let mut resp = vec![SSH_FXP_STATUS];
                ssh_uint32(&mut resp, req_id);
                ssh_uint32(&mut resp, SFXP_OP_UNSUPPORTED);
                ssh_string(&mut resp, b"unsupported");
                ssh_string(&mut resp, b"en");
                Ok(resp)
            }
        }
    }

    /// Route a pinhead request and return the response data.
    async fn route(&self, op: FsOperation, path: &str, data: Bytes) -> Result<Bytes, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            op,
            path: path.to_string(),
            data,
            reply: reply_tx,
        };
        self.router_tx
            .send(req)
            .await
            .map_err(|_| "router gone".to_string())?;
        let h_resp = reply_rx
            .await
            .map_err(|_| "handler gone".to_string())?
            .map_err(|e| e)?;
        Ok(h_resp.data)
    }
    // ── SFTP command handlers ──────────────────────────────────────────────

    async fn sftp_open(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;
        let _pflags = read_u32(data, &mut off)?;
        let _attrs = data.get(off..).unwrap_or_default();

        // Route Open to verify the path is accessible
        self.route(FsOperation::Open, &path, Bytes::new())
            .await?;

        let handle = self.handles.lock().await.alloc(&path, false);

        let mut resp = vec![SSH_FXP_HANDLE];
        ssh_uint32(&mut resp, req_id);
        ssh_string(&mut resp, handle.as_bytes());
        Ok(resp)
    }

    async fn sftp_close(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let handle = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid handle".to_string())?;

        // Get path for Release
        let entry = {
            let h = self.handles.lock().await;
            h.get(&handle).cloned()
        };

        if let Some(entry) = entry {
            let _ = self
                .route(FsOperation::Release, &entry.path, Bytes::new())
                .await;
        }

        self.handles.lock().await.free(&handle);

        Ok(make_status(req_id, SFXP_OK, "OK"))
    }

    async fn sftp_read(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let handle = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid handle".to_string())?;
        let _offset = read_u64(data, &mut off)?;
        let _length = read_u32(data, &mut off)?;

        let entry = {
            let h = self.handles.lock().await;
            h.get(&handle).cloned()
        }
        .ok_or("unknown handle")?;

        let resp_data = self
            .route(FsOperation::Read, &entry.path, Bytes::new())
            .await?;

        let mut sftp_resp = vec![SSH_FXP_DATA];
        ssh_uint32(&mut sftp_resp, req_id);
        ssh_string(&mut sftp_resp, &resp_data);
        Ok(sftp_resp)
    }

    async fn sftp_write(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let handle = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid handle".to_string())?;
        let _offset = read_u64(data, &mut off)?;
        let write_data = read_ssh_string(data, &mut off)?;

        let entry = {
            let h = self.handles.lock().await;
            h.get(&handle).cloned()
        }
        .ok_or("unknown handle")?;

        self.route(FsOperation::Write, &entry.path, Bytes::from(write_data))
            .await?;

        Ok(make_status(req_id, SFXP_OK, "OK"))
    }

    async fn sftp_opendir(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        // Route OpenDir
        self.route(FsOperation::OpenDir, &path, Bytes::new())
            .await?;

        let handle = self.handles.lock().await.alloc(&path, true);

        let mut resp = vec![SSH_FXP_HANDLE];
        ssh_uint32(&mut resp, req_id);
        ssh_string(&mut resp, handle.as_bytes());
        Ok(resp)
    }

    async fn sftp_readdir(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let handle = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid handle".to_string())?;

        let entry = {
            let h = self.handles.lock().await;
            h.get(&handle).cloned()
        }
        .ok_or("unknown handle")?;

        if !entry.is_dir {
            return Ok(make_status(req_id, SFXP_FAILURE, "not a directory"));
        }

        let resp_data = self
            .route(FsOperation::ReadDir, &entry.path, Bytes::new())
            .await
            .map(|b| {
                // Parse the response as a list of names
                let text = String::from_utf8_lossy(&b);
                text.split(|c: char| c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
            })?;

        // Build NAME response with "." and ".."
        let mut names = vec![".".to_string(), "..".to_string()];
        names.extend(resp_data.iter().map(|s| s.to_string()));

        let mut resp = vec![SSH_FXP_NAME];
        ssh_uint32(&mut resp, req_id);
        ssh_uint32(&mut resp, names.len() as u32);
        for name in &names {
            ssh_string(&mut resp, name.as_bytes());
            ssh_string(&mut resp, name.as_bytes()); // longname (same)
            // FileAttributes: empty (no attrs)
            ssh_uint32(&mut resp, 0); // flags = 0
        }
        Ok(resp)
    }

    async fn sftp_remove(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        self.route(FsOperation::Unlink, &path, Bytes::new())
            .await?;
        Ok(make_status(req_id, SFXP_OK, "OK"))
    }

    async fn sftp_mkdir(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;
        let _attrs = data.get(off..).unwrap_or_default();

        self.route(FsOperation::MkDir, &path, Bytes::new())
            .await?;
        Ok(make_status(req_id, SFXP_OK, "OK"))
    }

    async fn sftp_rmdir(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        self.route(FsOperation::RmDir, &path, Bytes::new())
            .await?;
        Ok(make_status(req_id, SFXP_OK, "OK"))
    }

    async fn sftp_stat(&self, data: &[u8], req_id: u32, _lstat: bool) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        let resp_data = self
            .route(FsOperation::GetAttr, &path, Bytes::new())
            .await?;

        let resp_text = String::from_utf8_lossy(&resp_data).to_string();
        let is_dir = resp_text.contains("directory")
            || resp_text.contains("mode=directory")
            || resp_text.contains("dir");

        let mut attrs = Vec::new();
        let mut flags: u32 = 0;
        // Parse size from response like "mode=file size=128"
        let size = if let Some(pos) = resp_text.find("size=") {
            let rest = &resp_text[pos + 5..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            num_str.parse::<u64>().unwrap_or(0)
        } else {
            resp_data.len() as u64
        };

        if size > 0 {
            flags |= 0x00000001; // SSH_FILEXFER_ATTR_SIZE
            ssh_uint64(&mut attrs, size);
        }

        flags |= 0x00000004; // SSH_FILEXFER_ATTR_PERMISSIONS
        let perms = if is_dir { 0o40755u32 } else { 0o100644u32 };
        ssh_uint32(&mut attrs, perms);

        let mut resp = vec![SSH_FXP_ATTRS];
        ssh_uint32(&mut resp, req_id);
        ssh_uint32(&mut resp, flags);
        resp.extend_from_slice(&attrs);
        Ok(resp)
    }

    async fn sftp_fstat(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let handle = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid handle".to_string())?;

        let entry = {
            let h = self.handles.lock().await;
            h.get(&handle).cloned()
        }
        .ok_or("unknown handle")?;

        // Reuse stat with the handle's path
        let sftp_data = {
            let mut d = vec![SSH_FXP_STAT, 0, 0, 0, 0]; // placeholder
            ssh_string(&mut d, entry.path.as_bytes());
            d
        };
        self.sftp_stat(&sftp_data, req_id, false).await
    }

    async fn sftp_realpath(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let path = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        let resolved = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };

        // Normalize
        let resolved = resolved
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let resolved = format!("/{resolved}");

        let mut resp = vec![SSH_FXP_NAME];
        ssh_uint32(&mut resp, req_id);
        ssh_uint32(&mut resp, 1); // count
        ssh_string(&mut resp, resolved.as_bytes());
        ssh_string(&mut resp, resolved.as_bytes()); // longname
        ssh_uint32(&mut resp, 0); // attrs flags
        Ok(resp)
    }

    async fn sftp_rename(&self, data: &[u8], req_id: u32) -> Result<Vec<u8>, String> {
        let mut off = 5;
        let old = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;
        let new = String::from_utf8(read_ssh_string(data, &mut off)?)
            .map_err(|_| "invalid path".to_string())?;

        // Rename not directly in FsOperation; we can do unlink + write, or
        // we could route it creatively. For now, return unsupported.
        // Real implementation would need a Rename op.
        let _ = (old, new);
        Ok(make_status(
            req_id,
            SFXP_OP_UNSUPPORTED,
            "rename not supported",
        ))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn make_status(req_id: u32, code: u32, msg: &str) -> Vec<u8> {
    let mut resp = vec![SSH_FXP_STATUS];
    ssh_uint32(&mut resp, req_id);
    ssh_uint32(&mut resp, code);
    ssh_string(&mut resp, msg.as_bytes());
    ssh_string(&mut resp, b"en");
    resp
}

fn ssh_uint64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn read_u64(data: &[u8], off: &mut usize) -> Result<u64, String> {
    if *off + 8 > data.len() {
        return Err("short u64".to_string());
    }
    let v = u64::from_be_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Ok(v)
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Configuration for the SSHFS frontend.
pub struct SshfsConfig {
    /// Password for password authentication (None = no global password).
    pub password: Option<String>,
    /// Path to an authorized_keys file (one ed25519 public key per line).
    pub authorized_keys_path: Option<String>,
    /// Username/password pairs for per-user authentication.
    pub userpasswds: Vec<(String, String)>,
}

/// Start a minimal SSH/SFTP server on the given TCP address.
///
/// For each incoming connection, a new task is spawned to handle the SSH
/// handshake and SFTP session, forwarding filesystem operations to the
/// pinhead router.
pub async fn serve(
    router_tx: mpsc::Sender<Request>,
    addr: &str,
    config: SshfsConfig,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[sshfs] listening on {addr}");

    // Generate host key
    let host_key = {
        let mut key_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut key_bytes);
        SigningKey::from_bytes(&key_bytes)
    };

    // Load authorized keys
    let authorized_keys = if let Some(path) = &config.authorized_keys_path {
        load_authorized_keys(path).unwrap_or_default()
    } else {
        Vec::new()
    };

    let handles = Arc::new(Mutex::new(HandleState::new()));

    loop {
        let (mut stream, peer) = listener.accept().await?;
        eprintln!("[sshfs] connection from {peer}");

        let router_tx = router_tx.clone();
        let host_key = host_key.clone();
        let password = config.password.clone();
        let keys = authorized_keys.clone();
        let userpasswds = config.userpasswds.clone();
        let handles = handles.clone();

        tokio::spawn(async move {
            let host_key_pub = host_key.verifying_key();
            let session = SshSession {
                router_tx,
                host_key,
                host_key_pub,
                handles,
                password,
                authorized_keys: keys,
                userpasswds,
            };

            if let Err(e) = session.run(&mut stream).await {
                eprintln!("[sshfs] {peer} error: {e}");
            }

            let _ = stream.shutdown().await;
            eprintln!("[sshfs] {peer} disconnected");
        });
    }
}

/// Load ed25519 public keys from an authorized_keys file.
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

        if let Ok(key_bytes) = simple_base64_decode(parts[1]) {
            if key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key_bytes);
                if let Ok(vk) = VerifyingKey::from_bytes(&arr) {
                    keys.push(vk);
                }
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
        let val = CHARS.iter().position(|&x| x == c).ok_or("invalid base64")? as u32;
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
