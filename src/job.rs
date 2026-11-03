//! Background processing. One worker thread owns the engine work; the GUI sends jobs and receives `Event`s,
//! so it never blocks. Plain std threads + channels: no async runtime needed for file-at-a-time jobs.
//! Progress is per-file/per-stage because PKON's API is one-shot (no intra-file progress callback).
use crate::{engine::Engine, platform};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

pub struct Job {
    pub id: u64,
    pub input: PathBuf,
    pub dest_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Compressing,
    Verifying,
    Writing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started {
        id: u64,
    },
    Stage {
        id: u64,
        stage: Stage,
    },
    Done {
        id: u64,
        output: PathBuf,
        in_bytes: u64,
        out_bytes: u64,
    },
    Failed {
        id: u64,
        error: String,
    },
    Cancelled {
        id: u64,
    },
}

pub struct JobRunner {
    tx: Option<Sender<Job>>,
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl JobRunner {
    pub fn spawn(engine: Arc<dyn Engine>) -> Self {
        let (tx, rx) = channel::<Job>();
        let (etx, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let worker = std::thread::spawn(move || {
            for job in rx {
                let id = job.id;
                let ev = if c.load(Ordering::Relaxed) {
                    Event::Cancelled { id }
                } else {
                    run(&*engine, job, &etx)
                };
                if etx.send(ev).is_err() {
                    break; // receiver side gone
                }
            }
        });
        Self {
            tx: Some(tx),
            events,
            cancel,
            worker: Some(worker),
        }
    }

    pub fn submit(&self, job: Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }

    /// Queued jobs report `Cancelled`; the job already running finishes (the engine API cannot be interrupted).
    pub fn cancel_pending(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Finish queued work and stop the worker.
    pub fn shutdown(mut self) {
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn run(engine: &dyn Engine, job: Job, etx: &Sender<Event>) -> Event {
    let id = job.id;
    let stage = |s| {
        let _ = etx.send(Event::Stage { id, stage: s });
    };
    let _ = etx.send(Event::Started { id });
    let fail = |e: String| Event::Failed { id, error: e };

    stage(Stage::Compressing);
    let bytes = match engine.compress(&job.input) {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    // Lossless is the product's promise: never write an output that was not proven to decode back exactly.
    stage(Stage::Verifying);
    if let Err(e) = engine.verify(&job.input, &bytes) {
        return fail(format!("verification failed, nothing written: {e}"));
    }
    stage(Stage::Writing);
    let out = platform::unique_output_path(&job.dest_dir, &job.input, engine.extension());
    if let Err(e) = platform::write_new_file(&out, &bytes) {
        return fail(format!("cannot write {}: {e}", out.display()));
    }
    let in_bytes = std::fs::metadata(&job.input).map(|m| m.len()).unwrap_or(0);
    Event::Done {
        id,
        output: out,
        in_bytes,
        out_bytes: bytes.len() as u64,
    }
}
