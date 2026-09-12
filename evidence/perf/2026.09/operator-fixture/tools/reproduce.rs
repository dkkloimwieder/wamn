use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::sync::atomic::{AtomicBool, Ordering};

// The runner inserts Fixture and its script method from the measured source.
mod fixture {
    use super::*;
    include!("fixture.rs");
    pub fn write(root: PathBuf, name: &str) -> PathBuf {
        Fixture { root }.script(name, "exit 0")
    }
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("owned scratch directory"));
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(5));
    let threads: Vec<_> = (0..4).map(|_| {
        let stop = Arc::clone(&stop);
        let ready = Arc::clone(&ready);
        std::thread::spawn(move || {
            ready.wait();
            while !stop.load(Ordering::Relaxed) {
                let status = Command::new("/bin/true").stdin(Stdio::null()).status().expect("fork competitor");
                assert!(status.success());
            }
        })
    }).collect();
    ready.wait();
    let mut failure = None;
    let mut attempts = 0;
    for number in 0..2000 {
        let path = fixture::write(root.clone(), &format!("script-{number}"));
        attempts += 1;
        match Command::new(&path).stdin(Stdio::null()).status() {
            Ok(status) => assert!(status.success()),
            Err(error) => { failure = Some(error); break; }
        }
        fs::remove_file(path).expect("remove owned completed script");
    }
    stop.store(true, Ordering::Relaxed);
    for thread in threads { thread.join().expect("competitor completed"); }
    match failure {
        Some(error) => {
            println!("attempts={attempts} errno={:?} error={error}", error.raw_os_error());
            std::process::exit(if error.raw_os_error() == Some(26) { 26 } else { 1 });
        }
        None => println!("attempts={attempts} errno=none"),
    }
}
