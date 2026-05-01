use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use matchit::Router as MatchRouter;
use tokio::sync::{mpsc, oneshot};

use crate::fsop::FsOperation;
use crate::handler::HandlerRequest;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metadata stored per route in the matchit trie.
#[derive(Clone)]
pub struct RouteMeta {
    /// Operation → handler-name map.
    /// Key `"*"` = handles all operations (wildcard).
    pub handlers: HashMap<String, String>,
    pub pattern: String,
}

/// A request sent from a frontend (FUSE / 9p) to the router task.
pub struct Request {
    /// The filesystem operation being performed.
    pub op: FsOperation,
    /// The path being operated on (e.g. `/foo/bar.txt`).
    pub path: String,
    /// Payload data (e.g. bytes to write).
    pub data: Bytes,
    /// Channel to send the response back to the frontend.
    pub reply: oneshot::Sender<Result<Response, String>>,
}

/// A response sent from the router back to the frontend.
pub type Response = crate::handler::HandlerResponse;

// ---------------------------------------------------------------------------
// Router internals
// ---------------------------------------------------------------------------

/// The path trie, wrapped for safe shared access.
struct RouterInner {
    trie: MatchRouter<RouteMeta>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create an empty path router.
pub fn new() -> PathRouterBuilder {
    PathRouterBuilder {
        trie: MatchRouter::new(),
    }
}

/// Builder: register routes, then build into a running task.
pub struct PathRouterBuilder {
    trie: MatchRouter<RouteMeta>,
}

impl PathRouterBuilder {
    /// Register a path pattern with per-operation handler metadata.
    ///
    /// `meta.handlers` maps operation names (e.g. "lookup", "read")
    /// to handler labels.  Use `"*"` as the key for a wildcard that
    /// matches any operation not otherwise registered.
    ///
    /// Patterns follow the matchit syntax:
    /// - `/users/{id}` matches `/users/42` with `params["id"] = "42"`
    /// - `/*path` catches all remaining segments
    pub fn register(
        &mut self,
        pattern: impl Into<String>,
        meta: RouteMeta,
    ) -> Result<&mut Self, String> {
        self.trie
            .insert(pattern.into(), meta)
            .map_err(|e| format!("route registration failed: {e}"))?;
        Ok(self)
    }

    /// Consume the builder and spawn the router task.
    ///
    /// Returns:
    /// - `mpsc::Sender<Request>` — frontends send requests here
    /// - `tokio::task::JoinHandle<()>` — the router task handle
    pub fn build(
        self,
        handler_tx: mpsc::Sender<HandlerRequest>,
    ) -> (mpsc::Sender<Request>, tokio::task::JoinHandle<()>) {
        let (req_tx, req_rx) = mpsc::channel(128);
        let inner = Arc::new(RouterInner { trie: self.trie });

        let handle = tokio::spawn(run_router(inner, req_rx, handler_tx));

        (req_tx, handle)
    }
}

// ---------------------------------------------------------------------------
// Router task
// ---------------------------------------------------------------------------

async fn run_router(
    inner: Arc<RouterInner>,
    mut req_rx: mpsc::Receiver<Request>,
    handler_tx: mpsc::Sender<HandlerRequest>,
) {
    while let Some(req) = req_rx.recv().await {
        let result = dispatch(&inner, &handler_tx, req).await;

        // If dispatch itself failed (can't reach handler, etc.) the error
        // will have been sent back through the reply channel inside dispatch.
        if let Err(e) = result {
            eprintln!("[router] dispatch error: {e}");
        }
    }

    eprintln!("[router] request channel closed, shutting down");
}

/// Match the path, build a HandlerRequest, send it to the handler task, and
/// forward the response back to the frontend.
async fn dispatch(
    inner: &RouterInner,
    handler_tx: &mpsc::Sender<HandlerRequest>,
    req: Request,
) -> Result<(), String> {
    let op = req.op;
    let path = &req.path;
    let data = req.data;

    // 1. Match the path in the trie.
    let matched = match inner.trie.at(path) {
        Ok(m) => m,
        Err(e) => {
            let msg = format!("no route matches `{path}`: {e}");
            let _ = req.reply.send(Err(msg));
            return Ok(());
        }
    };

    let meta = matched.value.clone();
    let matched_pattern = meta.pattern.clone();
    let has_children = {
        let probe = format!("{path}/x");
        match inner.trie.at(&probe) {
            Ok(m) => m.value.pattern != matched_pattern && !m.value.pattern.contains('*'),
            Err(_) => false,
        }
    };

    // Select the handler_name based on the operation.
    let handler_name = meta.handlers.get(op.as_str()).or_else(|| {
        // Fall back to wildcard "*" handler (registered without ops).
        meta.handlers.get("*")
    });

    let handler_name = match handler_name {
        Some(n) => n.clone(),
        None => {
            let msg = format!(
                "route `{path}` exists but no handler for op `{}` and no wildcard",
                op.as_str()
            );
            let _ = req.reply.send(Err(msg));
            return Ok(());
        }
    };

    let params: HashMap<String, String> = matched
        .params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if has_children {
        use crate::fsop::FsOperation;
        match op {
            FsOperation::Open | FsOperation::Read | FsOperation::Write | FsOperation::Release => {
                let msg = format!("is a directory: `{path}`");
                let _ = req.reply.send(Err(msg));
                return Ok(());
            }
            _ => {}
        }
    }

    // 2. Build a handler request.
    let (reply_tx, reply_rx) = oneshot::channel();

    let hreq = HandlerRequest {
        params,
        data,
        handler_name,
        reply: reply_tx,
    };

    // 3. Send to the handler task.
    handler_tx
        .send(hreq)
        .await
        .map_err(|_| "handler task is gone".to_string())?;

    // 4. Wait for the handler's response and forward it.
    match reply_rx.await {
        Ok(Ok(mut resp)) => {
            resp.matched_pattern = Some(matched_pattern);
            resp.has_children = has_children;
            let _ = req.reply.send(Ok(resp));
            Ok(())
        }
        Ok(Err(e)) => {
            let _ = req.reply.send(Err(e));
            Ok(())
        }
        Err(_) => {
            let _ = req.reply.send(Err("handler did not respond".to_string()));
            Err("handler reply channel closed".to_string())
        }
    }
}
