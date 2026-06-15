// SPDX-License-Identifier: GPL-3.0-or-later

//! Locating the bundled AROS ROM. AROS (the AROS Research Operating System)
//! ships an open-source, freely redistributable Kickstart replacement for the
//! m68k that Copperline boots when the user has not supplied a ROM of their
//! own. Unlike a real Kickstart it can legally travel with the program, so it
//! is installed alongside the binary (the Homebrew formula drops it under
//! `share/copperline/aros/`) and located here at start-up.
//!
//! AROS is consumed as two halves, exactly as WinUAE and FS-UAE take it: a
//! 512 KiB main ROM that overlays at $F80000 like any Kickstart, plus a
//! 512 KiB extended ROM mapped at $E00000.

use std::path::{Path, PathBuf};

/// Main (Kickstart-replacement) ROM file name.
pub const AROS_MAIN_FILE: &str = "aros-amiga-m68k-rom.bin";
/// Extended ROM file name (maps at $E00000).
pub const AROS_EXT_FILE: &str = "aros-amiga-m68k-ext.bin";

/// A located pair of bundled AROS ROM files.
pub struct BundledAros {
    pub main: PathBuf,
    pub extended: PathBuf,
}

/// Search the conventional install locations for the bundled AROS ROM pair,
/// returning the first directory that holds both files. The order tried is:
/// an explicit `COPPERLINE_AROS_DIR` override; locations relative to the
/// running executable (a sibling `aros/`, a macOS `.app` `Resources/aros/`,
/// and a Homebrew/Unix `../share/copperline/aros/`); and finally the
/// source-tree `assets/aros/` so `cargo run` works during development.
pub fn find_bundled_aros() -> Option<BundledAros> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_AROS_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("aros"));
            // A macOS .app bundle puts the binary in Contents/MacOS, with
            // data in the sibling Contents/Resources. A Homebrew/Unix install
            // puts it in <prefix>/bin, with data under <prefix>/share.
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("aros"));
                dirs.push(parent.join("share").join("copperline").join("aros"));
            }
        }
    }

    // Development: running straight from the source tree.
    dirs.push(PathBuf::from("assets").join("aros"));

    dirs.into_iter().find_map(|dir| aros_pair_in(&dir))
}

/// When `dir` holds both AROS ROM files, return their paths.
fn aros_pair_in(dir: &Path) -> Option<BundledAros> {
    let main = dir.join(AROS_MAIN_FILE);
    let extended = dir.join(AROS_EXT_FILE);
    (main.is_file() && extended.is_file()).then_some(BundledAros { main, extended })
}
