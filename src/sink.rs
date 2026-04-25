use crate::task::{Signal, Task};
use tokio::sync::mpsc;

/// A sink task that collects final values and reports statistics.
pub struct Sink {
    name: String,
}

impl Sink {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }
}

impl Task for Sink {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        self: Box<Self>,
        mut input: mpsc::Receiver<i64>,
        _output: mpsc::Sender<i64>,
        mut ctrl: mpsc::Receiver<Signal>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let mut values: Vec<i64> = Vec::new();

            loop {
                tokio::select! {
                    biased;
                    sig = ctrl.recv() => {
                        if sig == Some(Signal::Shutdown) {
                            eprintln!("[{}] received shutdown", self.name);
                            break;
                        }
                    }
                    value = input.recv() => {
                        match value {
                            Some(v) => values.push(v),
                            None => {
                                // Upstream is done.
                                eprintln!("[{}] input channel closed", self.name);
                                break;
                            }
                        }
                    }
                }
            }

            // Report results.
            if values.is_empty() {
                eprintln!("[{}] no values received", self.name);
            } else {
                let sum: i64 = values.iter().sum();
                let avg = sum as f64 / values.len() as f64;
                eprintln!(
                    "[{}] received {} values: sum={}, avg={:.2}",
                    self.name,
                    values.len(),
                    sum,
                    avg
                );
            }
        }
    }
}
