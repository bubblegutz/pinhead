//! Worker pool for `!Send` Lua handler states.
//!
//! Uses `tokio_util::task::LocalPoolHandle` to spawn pinned workers that each
//! hold their own `mlua::Lua` state.  Bytecode is shared zero-copy across
//! workers via `Arc<SharedBytecodes>`.

use std::sync::Arc;
use std::time::Duration;

use mlua::Lua;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::task::LocalPoolHandle;

use crate::handler::{
    HandlerRequest, HandlerRuntime, SharedBytecodes, WorkerConfig,
};

/// Thread-safe worker pool that dispatches `HandlerRequest`s to pinned worker
/// tasks (each holding a `!Send` Lua state).
pub(crate) struct WorkerPool {
    pool: LocalPoolHandle,
    bytecodes: Arc<SharedBytecodes>,
    config: Arc<WorkerConfig>,
}

impl WorkerPool {
    /// Create a new worker pool.
    ///
    /// `max_helpers` controls the maximum number of pinned OS threads the
    /// `LocalPoolHandle` may use.
    pub fn new(
        max_helpers: usize,
        bytecodes: SharedBytecodes,
        config: WorkerConfig,
    ) -> Self {
        Self {
            pool: LocalPoolHandle::new(max_helpers),
            bytecodes: Arc::new(bytecodes),
            config: Arc::new(config),
        }
    }

    /// Spawn the dispatcher task.  Returns a `JoinHandle` that resolves when
    /// the dispatcher exits (channel closed).
    pub fn spawn_dispatcher(
        self,
        rx: mpsc::Receiver<HandlerRequest>,
    ) -> JoinHandle<()> {
        tokio::spawn(dispatcher_loop(self.pool, self.bytecodes, self.config, rx))
    }
}

// ── Dispatcher ──────────────────────────────────────────────────────────────

/// Round-robins requests across workers.  Monitors pending queue depth and
/// inflates/deflates the worker pool.
async fn dispatcher_loop(
    pool: LocalPoolHandle,
    bytecodes: Arc<SharedBytecodes>,
    config: Arc<WorkerConfig>,
    mut rx: mpsc::Receiver<HandlerRequest>,
) {
    let mut workers: Vec<(mpsc::UnboundedSender<HandlerRequest>, JoinHandle<()>)> =
        Vec::new();
    let mut next = 0usize;

    // Ensure at least min_workers exist.
    let min = config.min_workers.load(std::sync::atomic::Ordering::Acquire);
    for _ in 0..min {
        spawn_worker(&pool, &bytecodes, &config, &mut workers);
    }

    // Periodic cleanup interval.
    let mut cleanup_tick = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            maybe_req = rx.recv() => {
                let req = match maybe_req {
                    Some(r) => r,
                    None => {
                        // Router closed — drain.
                        eprintln!("[worker] router channel closed, shutting down dispatcher");
                        break;
                    }
                };

                // Check queue depth — inflate if growing.
                let max = config.max_workers.load(std::sync::atomic::Ordering::Acquire);
                if rx.len() > 5 && workers.len() < max {
                    spawn_worker(&pool, &bytecodes, &config, &mut workers);
                }

                // Round-robin dispatch.
                if workers.is_empty() {
                    eprintln!("[worker] no workers available, dropping request");
                    let _ = req.reply.send(Err("no workers available".into()));
                    continue;
                }
                next %= workers.len();
                if workers[next].0.send(req).is_err() {
                    // Worker died — remove and retry.
                    workers.swap_remove(next);
                    // Don't advance next — swap_remove replaces the slot.
                    continue;
                }
                next += 1;
            }

            _ = cleanup_tick.tick() => {
                // Remove finished workers.
                workers.retain(|(_, handle)| !handle.is_finished());

                // Replenish if below min.
                let min = config.min_workers.load(std::sync::atomic::Ordering::Acquire);
                let max = config.max_workers.load(std::sync::atomic::Ordering::Acquire);
                while workers.len() < min && workers.len() < max {
                    spawn_worker(&pool, &bytecodes, &config, &mut workers);
                }
            }
        }
    }

    // Drain remaining requests on shutdown.
    for (tx, _) in &workers {
        let _ = tx;
        // Workers will exit via TTL — just drop channels.
    }
    // Wait briefly for workers to finish their current request.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// Spawn a new pinned worker task and add it to `workers`.
fn spawn_worker(
    pool: &LocalPoolHandle,
    bytecodes: &Arc<SharedBytecodes>,
    config: &Arc<WorkerConfig>,
    workers: &mut Vec<(mpsc::UnboundedSender<HandlerRequest>, JoinHandle<()>)>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<HandlerRequest>();
    let bc = Arc::clone(bytecodes);
    let cfg = Arc::clone(config);
    let handle = pool.spawn_pinned(move || async move {
        worker_fn(bc, cfg, rx).await;
    });
    workers.push((tx, handle));
    eprintln!("[worker] spawned worker (total={})", workers.len());
}

// ── Worker task ─────────────────────────────────────────────────────────────

/// A single pinned worker.  Creates its own `Lua` state, loads bytecodes, and
/// processes requests until idle TTL expires.
async fn worker_fn(
    bytecodes: Arc<SharedBytecodes>,
    config: Arc<WorkerConfig>,
    mut rx: mpsc::UnboundedReceiver<HandlerRequest>,
) {
    let ttl_secs = config.ttl_secs.load(std::sync::atomic::Ordering::Acquire);
    let ttl = Duration::from_secs(ttl_secs);

    // Create fresh Lua state and load bytecodes.
    let runtime = match build_runtime(&bytecodes) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[worker] failed to build runtime: {e}");
            return;
        }
    };

    loop {
        tokio::select! {
            maybe_req = rx.recv() => {
                let req = match maybe_req {
                    Some(r) => r,
                    None => {
                        // Channel closed, shut down.
                        break;
                    }
                };
                let result = runtime.call_lua(&req);
                let _ = req.reply.send(result);
            }

            _ = tokio::time::sleep(ttl) => {
                // Idle timeout — worker exits gracefully.
                break;
            }
        }
    }
}

/// Create an `mlua::Lua` state and re-execute the script to build a runtime.
fn build_runtime(bytecodes: &SharedBytecodes) -> Result<HandlerRuntime, String> {
    let lua = Lua::new();
    HandlerRuntime::from_bytecodes(lua, bytecodes)
}
