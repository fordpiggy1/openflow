//! One OpenFlow per machine.
//!
//! Two copies would each register the same global hotkey (the second
//! registration fails silently on macOS), each open the same SQLite file, and
//! each draw an overlay. An advisory `flock` on a file in the application
//! directory is enough: the lock is released by the kernel when the process
//! exits, however it exits, so a crash cannot leave a stale lock behind the way
//! a pid file can.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const LOCK_FILE: &str = "openflow.lock";

/// Holds the lock for as long as it is alive.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Take the lock, or report that another copy already holds it.
    pub fn acquire(app_dir: &Path) -> Result<Self, String> {
        let path = app_dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| format!("Could not open {}: {}", path.display(), error))?;

        // SAFETY: `file` owns the descriptor and outlives the call.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if taken != 0 {
            return Err("OpenFlow is already running.".to_string());
        }
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second acquire must fail while the first is alive, and succeed once
    /// it is dropped. Without the second half, a lock that never released would
    /// pass just as well.
    #[test]
    fn the_lock_admits_one_holder_at_a_time() {
        let dir = std::env::temp_dir().join(format!(
            "openflow-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");

        let first = InstanceLock::acquire(&dir).expect("the first copy takes the lock");
        assert!(
            dir.join(LOCK_FILE).exists(),
            "the lock file is created in the app directory"
        );
        assert!(
            InstanceLock::acquire(&dir).is_err(),
            "a second copy must be refused while the first holds the lock"
        );

        drop(first);
        assert!(
            InstanceLock::acquire(&dir).is_ok(),
            "the lock must be free again once the holder is gone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
