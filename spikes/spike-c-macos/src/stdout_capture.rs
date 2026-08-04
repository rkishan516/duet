//! Redirects the process's OS-level stdout (fd 1) through a pipe so we can
//! programmatically read the "The Dart VM service is listening on
//! http://127.0.0.1:PORT/AUTHCODE/" line the Flutter engine prints once at
//! startup. There is no other API surface in this embedding for recovering
//! that URI (Spike A just eyeballed it in the terminal); the engine writes
//! it with a raw C-level print straight to fd 1, since it's linked into
//! this same process rather than being a subprocess we could pipe normally.
//!
//! IMPORTANT: once this is active, this process's own `println!`/`print!`
//! would also get swallowed into the same pipe (fd 1 is fd 1, Rust doesn't
//! know the difference). This module's caller must switch all of its own
//! diagnostic output to `eprintln!` (fd 2, left untouched) from this point
//! on. Every line read off the pipe is also echoed to `eprintln!` so nothing
//! is lost - you'll see `[engine-stdout] ...` lines in the real terminal.

use std::io::{BufRead, BufReader};
use std::os::fd::FromRawFd;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Must be called exactly once, as early as possible (before booting the
/// Flutter engine, and before any of our own `println!` calls).
pub fn capture_stdout_lines() -> Receiver<String> {
    let (tx, rx) = channel::<String>();
    unsafe {
        let mut fds: [libc::c_int; 2] = [0; 2];
        let rc = libc::pipe(fds.as_mut_ptr());
        assert_eq!(rc, 0, "pipe() failed: {}", std::io::Error::last_os_error());
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // Point fd 1 (stdout) at the write end of the pipe. Anything any code in
        // this process writes to stdout from now on - our own println!, or the
        // Flutter engine's internal logging - lands in the pipe instead of the
        // terminal.
        let dup_rc = libc::dup2(write_fd, 1);
        assert!(
            dup_rc >= 0,
            "dup2(write_fd, 1) failed: {}",
            std::io::Error::last_os_error()
        );
        // fd 1 now also refers to the pipe's write end; this extra fd is redundant.
        libc::close(write_fd);

        let read_file = std::fs::File::from_raw_fd(read_fd);
        thread::Builder::new()
            .name("stdout-capture".into())
            .spawn(move || {
                let reader = BufReader::new(read_file);
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            eprintln!("[engine-stdout] {l}");
                            if tx.send(l).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                eprintln!("[stdout-capture] reader thread exiting (pipe closed)");
            })
            .expect("failed to spawn stdout-capture thread");
    }
    rx
}

/// Blocks (up to `timeout`) scanning captured stdout lines for the engine's
/// VM service announcement, and returns the parsed base HTTP URL
/// (e.g. `http://127.0.0.1:52963/287U1GZ6lE8=/`, always with a trailing `/`).
pub fn wait_for_vm_service_uri(rx: &Receiver<String>, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(idx) = line.find("http://") {
                    let url_part = line[idx..].trim();
                    let url = url_part.trim_end_matches('/');
                    return Some(format!("{url}/"));
                }
            }
            Err(_) => return None,
        }
    }
}
