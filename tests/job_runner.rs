//! Q: does the background runner deliver ordered events, refuse to write unverified output, and keep the caller unblocked?
use kon_app::engine::Engine;
use kon_app::job::{Event, Job, JobRunner, Stage};
use std::path::Path;
use std::sync::Arc;

struct Fake {
    verify_ok: bool,
}
impl Engine for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn extension(&self) -> &'static str {
        "fake"
    }
    fn compress(&self, input: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(input).map_err(|e| e.to_string())
    }
    fn verify(&self, _: &Path, _: &[u8]) -> Result<(), String> {
        if self.verify_ok {
            Ok(())
        } else {
            Err("forced".into())
        }
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("kon-job-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn success_emits_ordered_stages_then_done() {
    let d = scratch("ok");
    let input = d.join("in file.bin");
    std::fs::write(&input, b"hello").unwrap();
    let r = JobRunner::spawn(Arc::new(Fake { verify_ok: true }));
    r.submit(Job {
        id: 7,
        input,
        dest_dir: d.clone(),
    });
    let evs: Vec<Event> = (0..5).map(|_| r.events.recv().unwrap()).collect();
    assert_eq!(evs[0], Event::Started { id: 7 });
    assert_eq!(
        evs[1],
        Event::Stage {
            id: 7,
            stage: Stage::Compressing
        }
    );
    assert_eq!(
        evs[2],
        Event::Stage {
            id: 7,
            stage: Stage::Verifying
        }
    );
    assert_eq!(
        evs[3],
        Event::Stage {
            id: 7,
            stage: Stage::Writing
        }
    );
    assert!(matches!(&evs[4], Event::Done { id: 7, output, .. } if output.exists()));
    r.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn failed_verification_writes_nothing() {
    let d = scratch("bad");
    let input = d.join("a.bin");
    std::fs::write(&input, b"x").unwrap();
    let r = JobRunner::spawn(Arc::new(Fake { verify_ok: false }));
    r.submit(Job {
        id: 1,
        input,
        dest_dir: d.clone(),
    });
    let last = loop {
        match r.events.recv().unwrap() {
            e @ (Event::Failed { .. } | Event::Done { .. }) => break e,
            _ => {}
        }
    };
    assert!(matches!(last, Event::Failed { .. }));
    r.shutdown();
    let outputs = std::fs::read_dir(&d)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "fake")
        })
        .count();
    assert_eq!(outputs, 0);
    std::fs::remove_dir_all(d).unwrap();
}
