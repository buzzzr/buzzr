//! Starting a program that was written to disk a moment ago.
//!
//! The kernel refuses to execute a file that is still open for writing by any process: `execve` answers `ETXTBSY`,
//! "Text file busy". Two ordinary things reach that state:
//!
//! * a plugin whose files changed — the host writes the new revision of the entry and starts it in the same breath;
//! * a copy of a program made while another thread of the process started a child of its own. Every start forks first,
//!   and the child inherits the write handle of whatever was open at that instant, so it holds the copy open until it
//!   reaches its own `exec`. This is how a suite of tests that each write a fake program and run it flakes: the write is
//!   in one test, the fork in another, and the start that loses the race fails.
//!
//! In both cases the writer is gone within microseconds, so a start that is refused is worth another try rather than an
//! error the caller has to explain. [`start`] is that try, over anything that starts a process — `std::process` or
//! `tokio::process` alike.

use std::io;
use std::time::{Duration, Instant};

/// How long one start may keep trying, and how long to wait between tries. Short: the window closes as soon as the
/// process holding the file reaches its `exec`.
const RETRY: Duration = Duration::from_millis(250);
const PAUSE: Duration = Duration::from_millis(10);

/// Run `attempt` — normally the `spawn` of a `Command` — trying again while the kernel says the program is still being
/// written. Every other failure comes back as it is, on the first try.
pub fn start<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let until = Instant::now() + RETRY;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(e) if busy(&e) && Instant::now() < until => std::thread::sleep(PAUSE),
            Err(e) => return Err(e),
        }
    }
}

/// Is this the kernel saying the file is still open for writing?
fn busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::ExecutableFileBusy
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn text_file_busy() -> io::Error {
        io::Error::new(io::ErrorKind::ExecutableFileBusy, "text file busy")
    }

    #[test]
    fn a_start_the_kernel_calls_busy_is_tried_again_until_it_works() {
        let tries = Cell::new(0);
        let started = start(|| {
            tries.set(tries.get() + 1);
            if tries.get() < 3 { Err(text_file_busy()) } else { Ok(tries.get()) }
        });
        assert_eq!(started.unwrap(), 3, "the third try got there");
        assert_eq!(tries.get(), 3, "and the two before it were tried");
    }

    #[test]
    fn any_other_failure_comes_back_at_once() {
        let tries = Cell::new(0);
        let failed: io::Result<()> = start(|| {
            tries.set(tries.get() + 1);
            Err(io::Error::new(io::ErrorKind::NotFound, "no such program"))
        });
        assert_eq!(failed.map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
        assert_eq!(tries.get(), 1, "a program that is not there is not going to appear");
    }

    #[test]
    fn a_busy_program_that_never_clears_is_an_error_and_not_a_hang() {
        let failed: io::Result<()> = start(|| Err(text_file_busy()));
        assert_eq!(failed.map_err(|e| e.kind()), Err(io::ErrorKind::ExecutableFileBusy));
    }

    #[cfg(unix)]
    #[test]
    fn the_kernel_s_busy_errno_is_the_failure_we_retry() {
        // ETXTBSY is 26 on Linux and the BSDs: this is the error the kernel really returns, so the arm above is not a
        // guess about how it maps into a kind.
        assert!(busy(&io::Error::from_raw_os_error(26)));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_still_open_for_writing_is_started_once_the_writer_lets_go() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("buzzr-spawn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("program");
        let mut handle = std::fs::File::create(&path).unwrap();
        handle.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Somebody still holds the file open for writing — which is the whole point: `exec` is refused until they let go,
        // and they let go a few milliseconds later, the way a process that just wrote a program does.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(handle);
        });

        let mut cmd = std::process::Command::new(&path);
        let mut child = start(|| cmd.spawn()).expect("the start waited for the writer");
        let status = child.wait().unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(dir).ok();
    }
}
