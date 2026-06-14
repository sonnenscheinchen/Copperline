// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached environment-variable access.
//!
//! Every COPPERLINE_* knob is a start-up setting, but some are consulted from very
//! hot paths (per instruction, per color clock, per device tick). A live
//! `std::env::var*` call takes a process-wide lock and scans, so reading one
//! millions of times a second pins the host CPU and -- on macOS -- starves the
//! audio thread of that same lock (cpal underruns). To make that class of bug
//! impossible, the whole environment is snapshotted once on first access and
//! every lookup reads from the snapshot with no further OS calls or locks.
//!
//! Consequence: variables are read exactly once (at first access). They are
//! start-up knobs, so that is the intended behaviour. Code that needs a runtime
//! "do this once" toggle must use its own latch, not `remove_var`.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;

fn snapshot() -> &'static HashMap<OsString, OsString> {
    static SNAPSHOT: OnceLock<HashMap<OsString, OsString>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| std::env::vars_os().collect())
}

/// Whether the variable is set (presence check), like `var_os(..).is_some()`.
pub fn flag(name: &str) -> bool {
    snapshot().contains_key(OsStr::new(name))
}

/// The variable's raw value, like `std::env::var_os`.
pub fn var_os(name: &str) -> Option<OsString> {
    snapshot().get(OsStr::new(name)).cloned()
}

/// The variable's value as UTF-8, like `std::env::var(..).ok()`.
pub fn var(name: &str) -> Option<String> {
    snapshot()
        .get(OsStr::new(name))
        .and_then(|v| v.to_str().map(str::to_owned))
}
