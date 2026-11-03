//! Cancellation semantics of the JobRunner. Each test answers one question about "Cancel remaining".
//! Slow fake engine => deterministic timing; one test uses the real PKON adapter on a mixed-format batch.
use kon_app::engine::{Engine, Pkon};
use kon_app::job::{Event, Job, JobRunner};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Slow {
    ms: u64,
    started: Mutex<Vec<String>>,
    fail_name: Option<&'static str>,
}
impl Slow {
    fn new(ms: u64) -> Arc<Self> {
        Arc::new(Self {
            ms,
            started: Mutex::new(vec![]),
            fail_name: None,
        })
    }
}
impl Engine for Slow {
    fn name(&self) -> &'static str {
        "slow"
    }
    fn extension(&self) -> &'static str {
        "slow"
    }
    fn compress(&self, input: &Path) -> Result<Vec<u8>, String> {
        let n = input.file_name().unwrap().to_string_lossy().into_owned();
        self.started.lock().unwrap().push(n.clone());
        std::thread::sleep(Duration::from_millis(self.ms));
        if self.fail_name == Some(n.as_str()) {
            return Err("forced failure".into());
        }
        std::fs::read(input).map_err(|e| e.to_string())
    }
    fn verify(&self, _: &Path, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kon-cancel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn inputs(d: &Path, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let p = d.join(format!("in{i}.bin"));
            std::fs::write(&p, format!("payload {i}")).unwrap();
            p
        })
        .collect()
}
fn submit_all(r: &JobRunner, files: &[PathBuf], dest: &Path) {
    for (i, f) in files.iter().enumerate() {
        r.submit(Job {
            id: i as u64,
            input: f.clone(),
            dest_dir: dest.to_path_buf(),
        });
    }
}
fn terminal_id(e: &Event) -> Option<u64> {
    match e {
        Event::Done { id, .. } | Event::Failed { id, .. } | Event::Cancelled { id } => Some(*id),
        _ => None,
    }
}
/// Wait for `Started{id}`.
fn wait_started(r: &JobRunner, id: u64) {
    loop {
        if matches!(r.events.recv_timeout(Duration::from_secs(5)).expect("no Started"), Event::Started { id: i } if i == id)
        {
            return;
        }
    }
}
/// Collect terminal events until `n` arrived or `max` elapsed; returns (events, time each arrived since `t0`).
fn collect(r: &JobRunner, n: usize, max: Duration, t0: Instant) -> Vec<(Event, Duration)> {
    let mut out = Vec::new();
    while out.len() < n {
        match r.events.recv_timeout(
            max.saturating_sub(t0.elapsed())
                .max(Duration::from_millis(1)),
        ) {
            Ok(e) if terminal_id(&e).is_some() => out.push((e, t0.elapsed())),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}
fn no_more_events(r: &JobRunner) {
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        r.events.try_recv().is_err(),
        "extra events after the batch settled"
    );
}

/// Q: when Cancel is clicked during a long job, are the queued jobs reported cancelled immediately — not only after the
/// running job finishes? (This is the reported bug: the button looked dead until the current file was done.)
#[test]
fn queued_jobs_are_cancelled_immediately_while_one_runs() {
    let d = scratch("imm");
    let files = inputs(&d, 4);
    let eng = Slow::new(1500);
    let r = JobRunner::spawn(eng.clone());
    submit_all(&r, &files, &d);
    wait_started(&r, 0);
    let t0 = Instant::now();
    r.cancel_pending();
    let evs = collect(&r, 4, Duration::from_secs(10), t0);
    let cancelled_at: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::Cancelled { .. }))
        .map(|(_, t)| *t)
        .collect();
    assert_eq!(
        cancelled_at.len(),
        3,
        "jobs 1..3 must be cancelled: {evs:?}"
    );
    assert!(cancelled_at.iter().all(|t| *t < Duration::from_millis(500)), "queued jobs were only cancelled after {cancelled_at:?} — the running job's 1.5 s blocked them");
    // The running job keeps the existing semantics: it finishes and is reported Done.
    assert!(evs
        .iter()
        .any(|(e, _)| matches!(e, Event::Done { id: 0, .. })));
    assert_eq!(
        eng.started.lock().unwrap().len(),
        1,
        "no queued job may start after cancel"
    );
    r.shutdown();
}

/// Q: cancel before any job starts / before anything is submitted: nothing runs, every job gets exactly one Cancelled.
#[test]
fn cancel_before_any_job_starts() {
    let d = scratch("before");
    let files = inputs(&d, 3);
    let eng = Slow::new(200);
    let r = JobRunner::spawn(eng.clone());
    r.cancel_pending();
    submit_all(&r, &files, &d);
    let evs = collect(&r, 3, Duration::from_secs(3), Instant::now());
    let ids: HashSet<u64> = evs.iter().map(|(e, _)| terminal_id(e).unwrap()).collect();
    assert_eq!(ids, HashSet::from([0, 1, 2]));
    assert!(evs
        .iter()
        .all(|(e, _)| matches!(e, Event::Cancelled { .. })));
    assert!(eng.started.lock().unwrap().is_empty());
    no_more_events(&r);
    r.shutdown();
    assert!(std::fs::read_dir(&d).unwrap().all(|e| e
        .unwrap()
        .path()
        .extension()
        .is_none_or(|x| x != "slow")));
}

/// Q: cancelling between two jobs (job 0 done, job 1 about to start) and after several completed jobs — do completed
/// and failed jobs keep their state, and does nothing further start?
#[test]
fn completed_and_failed_jobs_keep_their_state() {
    let d = scratch("after");
    let files = inputs(&d, 6);
    let eng = Arc::new(Slow {
        ms: 300,
        started: Mutex::new(vec![]),
        fail_name: Some("in1.bin"),
    });
    let r = JobRunner::spawn(eng.clone());
    submit_all(&r, &files, &d);
    // Wait until jobs 0 (done) and 1 (failed) are finished and job 2 is running.
    let mut done = vec![];
    wait_started(&r, 0);
    let t0 = Instant::now();
    while done.len() < 2 {
        let e = r.events.recv_timeout(Duration::from_secs(5)).unwrap();
        if terminal_id(&e).is_some() {
            done.push(e);
        }
    }
    wait_started(&r, 2);
    r.cancel_pending();
    let rest = collect(&r, 4, Duration::from_secs(5), t0);
    assert!(matches!(done[0], Event::Done { id: 0, .. }));
    assert!(matches!(done[1], Event::Failed { id: 1, .. }));
    let kinds: Vec<_> = rest
        .iter()
        .map(|(e, _)| {
            (
                terminal_id(e).unwrap(),
                matches!(e, Event::Cancelled { .. }),
            )
        })
        .collect();
    // 2 finishes (running when cancelled); 3,4,5 cancelled.
    assert_eq!(
        kinds
            .iter()
            .filter(|k| k.1)
            .map(|k| k.0)
            .collect::<HashSet<_>>(),
        HashSet::from([3, 4, 5])
    );
    assert!(rest
        .iter()
        .any(|(e, _)| matches!(e, Event::Done { id: 2, .. })));
    assert_eq!(
        eng.started.lock().unwrap().len(),
        3,
        "only jobs 0,1,2 may ever start"
    );
    no_more_events(&r);
    r.shutdown();
}

/// Q: do repeated cancel requests corrupt state or duplicate terminal events?
#[test]
fn repeated_cancel_is_idempotent() {
    let d = scratch("twice");
    let files = inputs(&d, 5);
    let r = JobRunner::spawn(Slow::new(400));
    submit_all(&r, &files, &d);
    wait_started(&r, 0);
    for _ in 0..12 {
        r.cancel_pending();
    }
    let evs = collect(&r, 5, Duration::from_secs(5), Instant::now());
    let mut ids: Vec<u64> = evs.iter().map(|(e, _)| terminal_id(e).unwrap()).collect();
    ids.sort();
    assert_eq!(ids, [0, 1, 2, 3, 4], "exactly one terminal event per job");
    no_more_events(&r);
}

/// Q: with the real PKON adapter and a mixed PNG/JPEG/DNG batch, does cancelling while a DNG is compressing stop the
/// rest at once, leave originals untouched, and write only the finished output?
#[test]
fn mixed_format_batch_cancel_with_real_engine() {
    let fx = PathBuf::from(std::env::var("KON_TEST_FIXTURES").expect("set KON_TEST_FIXTURES"));
    let dng = PathBuf::from(std::env::var("KON_TEST_DNG_DIR").expect("set KON_TEST_DNG_DIR"))
        .join("Nikon__D80__DSC_0143.dng");
    let files = vec![
        dng,
        fx.join("rgb.png"),
        fx.join("prog.jpg"),
        fx.join("gray.jpg"),
    ];
    let before: Vec<_> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let d = scratch("mixed");
    let r = JobRunner::spawn(Arc::new(Pkon));
    submit_all(&r, &files, &d);
    wait_started(&r, 0);
    let t0 = Instant::now();
    r.cancel_pending();
    let evs = collect(&r, 4, Duration::from_secs(120), t0);
    let cancelled: Vec<_> = evs
        .iter()
        .filter(|(e, _)| matches!(e, Event::Cancelled { .. }))
        .collect();
    assert_eq!(cancelled.len(), 3);
    assert!(
        cancelled.iter().all(|(_, t)| *t < Duration::from_secs(1)),
        "queued PNG/JPEG jobs waited for the DNG"
    );
    assert!(
        evs.iter()
            .any(|(e, _)| matches!(e, Event::Done { id: 0, .. })),
        "running DNG finishes under existing semantics"
    );
    no_more_events(&r);
    r.shutdown();
    let outs: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(outs, ["Nikon__D80__DSC_0143.pkon"]);
    let after: Vec<_> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert!(before == after, "source files must be untouched");
    let _ = AtomicUsize::new(0).load(Ordering::Relaxed);
}
