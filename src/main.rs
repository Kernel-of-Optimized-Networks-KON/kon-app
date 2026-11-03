//! Temporary headless front end over the same `JobRunner` the GUI will use: `kon-app <dest-dir> <png>...`
use kon_app::{
    engine::Pkon,
    job::{Event, Job, JobRunner},
};
use std::{path::PathBuf, sync::Arc};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(dest) = args.next().map(PathBuf::from) else {
        eprintln!("usage: kon-app <dest-dir> <image.png>...");
        std::process::exit(2);
    };
    let runner = JobRunner::spawn(Arc::new(Pkon));
    let mut pending = 0;
    for (i, input) in args.enumerate() {
        runner.submit(Job {
            id: i as u64,
            input: input.into(),
            dest_dir: dest.clone(),
        });
        pending += 1;
    }
    let mut failed = false;
    while pending > 0 {
        match runner.events.recv().expect("worker died") {
            Event::Done {
                output,
                in_bytes,
                out_bytes,
                ..
            } => {
                pending -= 1;
                println!(
                    "OK  {} ({in_bytes} -> {out_bytes} bytes, verified lossless)",
                    output.display()
                );
            }
            Event::Failed { id, error } => {
                pending -= 1;
                failed = true;
                eprintln!("FAIL job {id}: {error}");
            }
            Event::Cancelled { .. } => pending -= 1,
            Event::Started { .. } | Event::Stage { .. } => {}
        }
    }
    runner.shutdown();
    std::process::exit(failed as i32);
}
