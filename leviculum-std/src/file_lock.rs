//! Advisory file locks through libc's `flock`, for every Unix including
//! Android.
//!
//! Rust's std has file locking (`File::try_lock` and friends) on Linux,
//! but NOT on Android: there every call answers `Unsupported`. That is
//! how the phone's own mailbox died at its very first step — its
//! `delivery.lock` could not be taken, on every Android device, always
//! (order 1798, render walk 2026-11-13 finding 2). Bionic's `flock` is
//! the same system call std uses on Linux, so a desktop process and the
//! phone agree on the lock's semantics; only the wrapper was missing.
//!
//! Non-Unix targets keep std's own locking, which is supported there.

use std::fs::File;

/// What one non-blocking lock attempt came to.
#[derive(Debug)]
pub enum TryLock {
    /// The lock is ours; it falls with the file (or [`unlock`]).
    Held,
    /// Another holder is in the way.
    Busy,
    /// The attempt itself failed, in the OS's words.
    Failed(String),
}

/// Try the exclusive lock on `file`, without blocking.
pub fn exclusive(file: &File) -> TryLock {
    lock(file, Op::Exclusive)
}

/// Try the shared lock on `file`, without blocking.
pub fn shared(file: &File) -> TryLock {
    lock(file, Op::Shared)
}

enum Op {
    Exclusive,
    Shared,
    Release,
}

/// Let go of a lock on `file` before the file is dropped. Best effort:
/// dropping the file releases it too.
pub fn unlock(file: &File) {
    let _ = lock(file, Op::Release);
}

#[cfg(unix)]
fn lock(file: &File, op: Op) -> TryLock {
    use std::os::fd::AsRawFd;
    let op = match op {
        Op::Exclusive => libc::LOCK_EX | libc::LOCK_NB,
        Op::Shared => libc::LOCK_SH | libc::LOCK_NB,
        Op::Release => libc::LOCK_UN,
    };
    // SAFETY: flock takes a descriptor the borrowed `File` keeps open
    // across the call, and an operation flag; it touches no memory.
    if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
        return TryLock::Held;
    }
    let error = std::io::Error::last_os_error();
    match error.kind() {
        std::io::ErrorKind::WouldBlock => TryLock::Busy,
        _ => TryLock::Failed(error.to_string()),
    }
}

#[cfg(not(unix))]
fn lock(file: &File, op: Op) -> TryLock {
    let outcome = match op {
        Op::Exclusive => file.try_lock(),
        Op::Shared => file.try_lock_shared(),
        Op::Release => {
            return file.unlock().map_or_else(
                |error| TryLock::Failed(error.to_string()),
                |()| TryLock::Held,
            )
        }
    };
    match outcome {
        Ok(()) => TryLock::Held,
        Err(std::fs::TryLockError::WouldBlock) => TryLock::Busy,
        Err(std::fs::TryLockError::Error(error)) => TryLock::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The semantics `loquax`'s delivery lock leans on: one exclusive
    /// holder, shared holders beside each other, and release by unlock
    /// as by drop.
    #[test]
    fn exclusive_excludes_and_shared_share() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("lock");
        let a = File::create(&path).expect("a");
        let b = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .expect("b");
        assert!(matches!(exclusive(&a), TryLock::Held));
        assert!(matches!(exclusive(&b), TryLock::Busy));
        assert!(matches!(shared(&b), TryLock::Busy));
        unlock(&a);
        assert!(matches!(shared(&a), TryLock::Held));
        assert!(matches!(shared(&b), TryLock::Held));
        assert!(matches!(exclusive(&a), TryLock::Busy));
        drop(b);
        // `a` still holds its shared lock; upgrading is flock's business,
        // but releasing and retaking exclusively must work.
        unlock(&a);
        assert!(matches!(exclusive(&a), TryLock::Held));
    }
}
