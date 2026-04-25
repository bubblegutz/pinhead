use crate::task::{Signal, Task};
use tokio::sync::mpsc;
use tokio::time::{self, Duration};

/// A source task that emits sequential integers on a configurable tick.
pub struct Source {
    name: String,
    start: i64,
    count: u64,
    interval: Duration,
}

impl Source {
    pub fn new(name: &str, start: i64, count: u64, interval: Duration) -> Self {
        Self {
            name: name.to_owned(),
            start,
            count,
            interval,
        }
    }
}

impl Task for Source {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        self: Box<Self>,
        _input: mpsc::Receiver<i64>,
        output: mpsc::Sender<i64>,
        mut ctrl: mpsc::Receiver<Signal>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let end = self.start + self.count as i64 - 1;
            for value in self.start..=end {
                // Race between sending the next value and receiving a shutdown signal.
                tokio::select! {
                    biased;
                    sig = ctrl.recv() => {
                        if sig == Some(Signal::Shutdown) {
                            eprintln!("[{}] received shutdown", self.name);
                            return;
                        }
                    }
                    result = output.send(value) => {
                        if result.is_err() {
                            eprintln!("[{}] output channel closed", self.name);
                            return;
                        }
                    }
                }
                time::sleep(self.interval).await;
            }
            eprintln!("[{}] finished emitting {} values", self.name, self.count);
        }
    }
}
