//! Background processing. One worker thread owns the engine work; the GUI sends jobs and receives `Event`s,
//! so it never blocks. Plain std threads + channels: no async runtime needed for file-at-a-time jobs.
//! Progress is per-file/per-stage because PKON's API is one-shot (no intra-file progress callback).
use crate::{engine::Engine, platform};
use std::path::PathBuf;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
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

/// Queue shared by the caller and the worker. The worker takes a job (= decides to start it) and `cancel_pending`
/// drains the queue under the same lock, so a job is either already running or reported `Cancelled` — never both,
/// and nothing can start after a cancel has been accepted.
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    cancelled: bool,
    closed: bool,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

pub struct JobRunner {
    shared: Arc<Shared>,
    etx: Sender<Event>,
    pub events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
}

impl JobRunner {
    pub fn spawn(engine: Arc<dyn Engine>) -> Self {
        let (etx, events) = channel();
        let shared = Arc::new(Shared { q: Mutex::new(Queue::default()), cv: Condvar::new() });
        let (sh, tx) = (shared.clone(), etx.clone());
        let worker = std::thread::spawn(move || loop {
            let job = {
                let mut q = sh.q.lock().unwrap();
                loop {
                    if let Some(j) = q.jobs.pop_front() {
                        break j;
                    }
                    if q.closed {
                        return;
                    }
                    q = sh.cv.wait(q).unwrap();
                }
            };
            let ev = run(&*engine, job, &tx);
            if tx.send(ev).is_err() {
                break; // receiver side gone
            }
        });
        Self { shared, etx, events, worker: Some(worker) }
    }

    /// After `cancel_pending`, newly submitted jobs are reported `Cancelled` straight away.
    pub fn submit(&self, job: Job) {
        let mut q = self.shared.q.lock().unwrap();
        if q.cancelled {
            drop(q);
            let _ = self.etx.send(Event::Cancelled { id: job.id });
        } else {
            q.jobs.push_back(job);
            self.shared.cv.notify_one();
        }
    }

    /// Every job that has not started is reported `Cancelled` immediately; the job already running finishes (the
    /// engine API cannot be interrupted) and reports its own result. Idempotent: repeat calls change nothing.
    pub fn cancel_pending(&self) {
        let drained: Vec<Job> = {
            let mut q = self.shared.q.lock().unwrap();
            q.cancelled = true;
            q.jobs.drain(..).collect()
        };
        for j in drained {
            let _ = self.etx.send(Event::Cancelled { id: j.id });
        }
    }

    /// Finish queued work and stop the worker.
    pub fn shutdown(mut self) {
        self.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    fn close(&self) {
        self.shared.q.lock().unwrap().closed = true;
        self.shared.cv.notify_all();
    }
}

impl Drop for JobRunner {
    fn drop(&mut self) {
        self.close();
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
