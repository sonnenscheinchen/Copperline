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

/// Whether any `COPPERLINE_*` variable is present at all. Every knob this
/// module exposes is named with that prefix, and the vast majority are
/// diagnostic switches consulted from very hot paths (per register write, per
/// memory access, per color clock). A normal run sets none of them, so the
/// snapshot lookup below would otherwise hash a ~25-character string with the
/// default SipHasher millions of times a second only to miss. Cache the
/// "are any of our knobs set?" answer once and short-circuit the common
/// no-knobs case to a single bool read.
fn any_copperline_var() -> bool {
    static ANY: OnceLock<bool> = OnceLock::new();
    *ANY.get_or_init(|| {
        snapshot().keys().any(|key| {
            key.to_str()
                .is_some_and(|name| name.starts_with("COPPERLINE"))
        })
    })
}

/// True when `name` is one of our knobs and we already know none are set, so
/// the caller can skip the snapshot hash entirely.
#[inline]
fn definitely_unset(name: &str) -> bool {
    name.starts_with("COPPERLINE") && !any_copperline_var()
}

/// Whether the variable is set (presence check), like `var_os(..).is_some()`.
#[inline]
pub fn flag(name: &str) -> bool {
    if definitely_unset(name) {
        return false;
    }
    snapshot().contains_key(OsStr::new(name))
}

/// The variable's raw value, like `std::env::var_os`.
#[inline]
pub fn var_os(name: &str) -> Option<OsString> {
    if definitely_unset(name) {
        return None;
    }
    snapshot().get(OsStr::new(name)).cloned()
}

/// The variable's value as UTF-8, like `std::env::var(..).ok()`.
#[inline]
pub fn var(name: &str) -> Option<String> {
    if definitely_unset(name) {
        return None;
    }
    snapshot()
        .get(OsStr::new(name))
        .and_then(|v| v.to_str().map(str::to_owned))
}
