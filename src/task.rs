use std::future::Future;
use tokio::sync::mpsc;

/// Control signal sent to tasks for graceful shutdown.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Shutdown,
}

/// A task is an independent unit of work that communicates only via channels.
/// Tasks run on tokio's M:N work-stealing scheduler — not on OS threads directly.
pub trait Task: Send + 'static {
    /// Human-readable name for logging/debugging.
    #[allow(dead_code)]
    fn name(&self) -> &str;

    /// Run the task. The task should read from `input`, write to `output`,
    /// and periodically check `ctrl` for shutdown signals.
    fn run(
        self: Box<Self>,
        input: mpsc::Receiver<i64>,
        output: mpsc::Sender<i64>,
        ctrl: mpsc::Receiver<Signal>,
    ) -> impl Future<Output = ()> + Send;
}

/// Spawn a task onto tokio's work-stealing scheduler.
pub fn spawn(
    task: impl Task,
    input: mpsc::Receiver<i64>,
    output: mpsc::Sender<i64>,
    ctrl: mpsc::Receiver<Signal>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let boxed = Box::new(task);
        boxed.run(input, output, ctrl).await;
    })
}
