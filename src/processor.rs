use crate::task::{Signal, Task};
use tokio::sync::mpsc;

/// A processor task that applies a transformation function to each value.
pub struct Processor {
    name: String,
    transform: fn(i64) -> i64,
}

impl Processor {
    pub fn new(name: &str, transform: fn(i64) -> i64) -> Self {
        Self {
            name: name.to_owned(),
            transform,
        }
    }
}

impl Task for Processor {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        self: Box<Self>,
        mut input: mpsc::Receiver<i64>,
        output: mpsc::Sender<i64>,
        mut ctrl: mpsc::Receiver<Signal>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            loop {
                tokio::select! {
                    biased;
                    sig = ctrl.recv() => {
                        if sig == Some(Signal::Shutdown) {
                            eprintln!("[{}] received shutdown", self.name);
                            return;
                        }
                    }
                    value = input.recv() => {
                        match value {
                            Some(v) => {
                                let result = (self.transform)(v);
                                eprintln!("[{}] {} -> {}", self.name, v, result);
                                if output.send(result).await.is_err() {
                                    eprintln!("[{}] output channel closed", self.name);
                                    return;
                                }
                            }
                            None => {
                                // Input channel closed — upstream is done.
                                eprintln!("[{}] input channel closed, exiting", self.name);
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}
