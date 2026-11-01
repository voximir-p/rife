// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::{APP_NAME, PROCESS_POLL_INTERVAL};
use anyhow::{Context, Result, ensure};
use rife_core::CancellationToken;
#[cfg(unix)]
use std::{fs, path::PathBuf};
use std::{fs::File, io, path::Path, thread};

/// Owns the output until processing and cleanup finish. Waiters reopen the marker
/// on each attempt, so its removal cannot leave them holding an obsolete lock.
pub(crate) struct OutputLock {
    _file: File,
    #[cfg(unix)]
    path: PathBuf,
}

enum Attempt {
    Acquired(OutputLock),
    Busy,
    #[cfg(unix)]
    Replaced,
}

impl OutputLock {
    pub(crate) fn acquire(
        output: &Path,
        overwrite: bool,
        cancel: &CancellationToken,
        on_wait: impl FnOnce(),
    ) -> Result<Self> {
        let mut lock_path = output.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_path = Path::new(&lock_path);
        let mut on_wait = Some(on_wait);
        loop {
            cancel.check()?;
            match Self::try_acquire(lock_path)
                .with_context(|| format!("could not lock output: {}", output.display()))?
            {
                Attempt::Acquired(lock) => {
                    // The guard already owns cleanup if cancellation arrived during open.
                    cancel.check()?;
                    return Ok(lock);
                }
                Attempt::Busy => {
                    ensure!(
                        overwrite,
                        "output is locked by another {APP_NAME} instance: {} (pass --overwrite to wait and replace it)",
                        output.display()
                    );
                    if let Some(notify) = on_wait.take() {
                        notify();
                    }
                    // Poll instead of blocking in the kernel so Ctrl+C remains responsive.
                    thread::sleep(PROCESS_POLL_INTERVAL);
                }
                #[cfg(unix)]
                Attempt::Replaced => continue,
            }
        }
    }

    #[cfg(windows)]
    fn try_acquire(path: &Path) -> io::Result<Attempt> {
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const ERROR_SHARING_VIOLATION: i32 = 32;

        // Exclusive sharing makes open + ownership atomic. No waiter keeps a
        // handle to this marker, and Windows deletes it even if the owner is killed.
        match File::options()
            .read(true)
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .create(true)
            .truncate(false)
            .open(path)
        {
            Ok(file) => Ok(Attempt::Acquired(Self { _file: file })),
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                Ok(Attempt::Busy)
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn try_acquire(path: &Path) -> io::Result<Attempt> {
        use fs2::FileExt;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) if current_marker(&file, path)? => Ok(Attempt::Acquired(Self {
                _file: file,
                path: path.to_owned(),
            })),
            // The previous owner may have unlinked the file between open and flock.
            // Never use a lock on an inode that is no longer at the marker path.
            Ok(()) => Ok(Attempt::Replaced),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                Ok(Attempt::Busy)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
fn current_marker(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let current = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let locked = file.metadata()?;
    Ok(current.dev() == locked.dev() && current.ino() == locked.ino())
}

#[cfg(unix)]
impl Drop for OutputLock {
    fn drop(&mut self) {
        // Unlink while still owning the OS lock. Acquirers validate inode identity
        // before entering; they retry if they opened this marker before its removal.
        if current_marker(&self._file, &self.path).unwrap_or(false) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        process::Command,
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")).unwrap()
    }

    #[test]
    fn locks_are_exclusive_per_output_and_reusable_after_drop() {
        let root = temp();
        let output = root.path().join("video \u{89c6}\u{9891}.mp4");
        let cancel = CancellationToken::default();
        let first =
            OutputLock::acquire(&output, false, &cancel, || panic!("unexpected wait")).unwrap();
        let error =
            OutputLock::acquire(&output, false, &cancel, || panic!("must fail immediately"))
                .err()
                .unwrap();
        assert_eq!(
            error.to_string(),
            format!(
                "output is locked by another {APP_NAME} instance: {} (pass --overwrite to wait and replace it)",
                output.display()
            )
        );
        let other =
            OutputLock::acquire(&root.path().join("other.mp4"), false, &cancel, || {}).unwrap();
        assert!(!output.exists());
        assert_eq!(
            fs::metadata(root.path().join("video \u{89c6}\u{9891}.mp4.lock"))
                .unwrap()
                .len(),
            0
        );
        drop(other);
        drop(first);
        assert!(!root.path().join("other.mp4.lock").exists());
        assert!(!root.path().join("video \u{89c6}\u{9891}.mp4.lock").exists());
        assert!(OutputLock::acquire(&output, false, &cancel, || {}).is_ok());
        assert!(!root.path().join("video \u{89c6}\u{9891}.mp4.lock").exists());
    }

    #[test]
    fn cancelling_a_waiter_does_not_release_the_owner() {
        let root = temp();
        let output = root.path().join("output.mp4");
        let owner =
            OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).unwrap();
        let cancel = CancellationToken::default();
        let signal = cancel.clone();
        let (started, waiting) = mpsc::channel();
        thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                OutputLock::acquire(&output, true, &cancel, || started.send(()).unwrap())
            });
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            signal.cancel();
            let start = Instant::now();
            assert!(waiter.join().unwrap().is_err());
            assert!(start.elapsed() < Duration::from_secs(1));
        });
        assert!(OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).is_err());
        assert!(root.path().join("output.mp4.lock").exists());
        drop(owner);
        assert!(!root.path().join("output.mp4.lock").exists());
        assert!(OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).is_ok());
    }

    #[test]
    fn overwrite_waits_until_the_owner_has_committed() {
        let root = temp();
        let output = root.path().join("output.mp4");
        let owner =
            OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).unwrap();
        let cancel = CancellationToken::default();
        let (started, waiting) = mpsc::channel();
        let (finished, acquired) = mpsc::channel();
        thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let guard =
                    OutputLock::acquire(&output, true, &cancel, || started.send(()).unwrap())
                        .unwrap();
                assert_eq!(fs::read_to_string(&output).unwrap(), "committed");
                finished.send(()).unwrap();
                guard
            });
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(acquired.recv_timeout(Duration::from_millis(100)).is_err());
            fs::write(&output, "committed").unwrap();
            drop(owner);
            acquired.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(waiter.join().unwrap());
        });
    }

    // Also runs as a child of the crash-recovery test. Without the private
    // environment setting it is a no-op during ordinary cargo test discovery.
    #[test]
    fn lock_owner_child() {
        let Some(root) = env::var_os("RIFE_TEST_LOCK_OWNER") else {
            return;
        };
        let root = Path::new(&root);
        let _lock = OutputLock::acquire(
            &root.join("output.mp4"),
            false,
            &CancellationToken::default(),
            || {},
        )
        .unwrap();
        fs::write(root.join("ready"), []).unwrap();
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn another_process_cannot_enter_and_forced_exit_releases_the_lock() {
        let root = temp();
        let output = root.path().join("output.mp4");
        let mut child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "output_lock::tests::lock_owner_child",
                "--nocapture",
            ])
            .env("RIFE_TEST_LOCK_OWNER", root.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let result = (|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !root.path().join("ready").exists() {
                ensure!(Instant::now() < deadline, "Child did not acquire the lock");
                ensure!(
                    child.try_wait()?.is_none(),
                    "Child exited before acquiring the lock"
                );
                thread::sleep(Duration::from_millis(10));
            }
            ensure!(
                OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).is_err(),
                "Child lock was ignored"
            );
            Ok::<_, anyhow::Error>(())
        })();
        let _ = child.kill();
        child.wait().unwrap();
        result.unwrap();
        #[cfg(windows)]
        assert!(!root.path().join("output.mp4.lock").exists());
        assert!(OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).is_ok());
        assert!(!root.path().join("output.mp4.lock").exists());
    }

    #[test]
    fn stale_markers_and_unwinding_leave_no_lock_file() {
        let root = temp();
        let output = root.path().join("output.mp4");
        let marker = root.path().join("output.mp4.lock");
        fs::write(&marker, []).unwrap();
        let result = std::panic::catch_unwind(|| {
            let _lock =
                OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).unwrap();
            panic!("simulated processing failure");
        });
        assert!(result.is_err());
        assert!(!marker.exists());
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(OutputLock::acquire(&output, false, &cancel, || {}).is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn concurrent_waiters_remain_exclusive_as_markers_are_recreated() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = temp();
        let output = root.path().join("output.mp4");
        let cancel = CancellationToken::default();
        let owner = OutputLock::acquire(&output, false, &cancel, || {}).unwrap();
        let active = AtomicUsize::new(0);
        let (started, waiting) = mpsc::channel();
        thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..8 {
                let started = started.clone();
                let output = &output;
                let active = &active;
                let cancel = &cancel;
                workers.push(scope.spawn(move || {
                    for _ in 0..4 {
                        let _lock =
                            OutputLock::acquire(output, true, cancel, || started.send(()).unwrap())
                                .unwrap();
                        assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                        thread::sleep(Duration::from_millis(1));
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    }
                }));
            }
            for _ in 0..8 {
                waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            drop(owner);
            for worker in workers {
                worker.join().unwrap();
            }
        });
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(!root.path().join("output.mp4.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unlinked_handles_do_not_match_a_recreated_marker() {
        let root = temp();
        let marker = root.path().join("output.mp4.lock");
        let old = File::create(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        let output = root.path().join("output.mp4");
        let current =
            OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).unwrap();
        assert!(!current_marker(&old, &marker).unwrap());
        assert!(OutputLock::acquire(&output, false, &CancellationToken::default(), || {}).is_err());
        drop(current);
        assert!(!marker.exists());
    }
}
