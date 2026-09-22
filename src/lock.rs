//! Single-writer guard.
//!
//! Every instance reads the same global interface counters, so two instances
//! accounting at once each attribute the same bytes and the totals inflate by
//! however many are running. Only the process holding this lock writes; any
//! other instance still renders, just from what's already stored.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

pub struct AccountingLock {
    /// Held open for the process lifetime; closing it releases the flock.
    _file: File,
}

/// Returns None when another instance is already accounting.
pub fn acquire() -> Option<AccountingLock> {
    let path = crate::store::data_dir().join("accounting.lock");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    // LOCK_NB so a second instance falls back to read-only instead of blocking.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then_some(AccountingLock { _file: file })
}
