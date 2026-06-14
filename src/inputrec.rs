// SPDX-License-Identifier: GPL-3.0-or-later

//! Live-input recording to the scripted-input format.
//!
//! While recording, every input event that reaches the emulated machine is
//! logged with its emulated timestamp and written out as a script of
//! `--press-after`-style directives (one per line, without the leading
//! dashes) that `--script FILE` replays deterministically. Combined with a
//! save state, a recording is a complete, shareable reproduction of a
//! manually driven session.
//!
//! Two capture styles feed the recorder:
//!
//! - Direct hooks for events with identity the bus state cannot expose
//!   afterwards: Amiga key transitions (`record_key`, from
//!   `handle_amiga_key_event`) and floppy inserts (`record_disk_insert`).
//! - A once-per-quantum `observe` of the live `InputState`, which diffs
//!   port-1 mouse buttons, the port-1 quadrature counters (wrapped u8
//!   deltas become `mouse-after` lines), and the port-2
//!   joystick/CD32-pad controls. Frame-rate granularity (~20 ms PAL) is
//!   the same resolution the replay side schedules at.
//!
//! Press/release pairs are merged into hold directives (`key-after` /
//! `click-after` / `joy-after`) keyed at the press time; controls still
//! held when the recording stops are closed at the final observed time.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::bus::InputState;

/// Minimum emitted hold so a press/release pair that lands inside one
/// quantum still asserts the control for at least one emulated frame on
/// replay.
const MIN_HOLD_MS: u32 = 20;

/// Reads one control's held state out of the live input.
type ControlRead = fn(&InputState) -> bool;

/// The port-2 controls the per-quantum diff watches: the canonical
/// `joy-after` name (accepted by `JoyButtonKind::parse`) and the
/// `InputState` field each one reads.
const PORT2_CONTROLS: [(&str, ControlRead); 11] = [
    ("up", |i| i.joy_up_port2),
    ("down", |i| i.joy_down_port2),
    ("left", |i| i.joy_left_port2),
    ("right", |i| i.joy_right_port2),
    ("red", |i| i.lmb_port2),
    ("blue", |i| i.rmb_port2),
    ("green", |i| i.cd32_green_port2),
    ("yellow", |i| i.cd32_yellow_port2),
    ("play", |i| i.cd32_play_port2),
    ("rwd", |i| i.cd32_rwd_port2),
    ("ffw", |i| i.cd32_ffw_port2),
];

const PORT1_BUTTONS: [(&str, ControlRead); 3] = [
    ("left", |i| i.lmb_port1),
    ("right", |i| i.rmb_port1),
    ("middle", |i| i.mmb_port1),
];

/// Default recording file name, timestamped like the screenshot/recorder
/// names.
pub fn auto_filename() -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("copperline-input-{secs}.clscript"))
}

pub struct InputRecorder {
    /// Emulated time of the most recent `observe`, used to close holds
    /// that are still open when the recording stops.
    last_secs: f64,
    /// Finished directives, keyed by the press/event time for sorting.
    lines: Vec<(f64, String)>,
    /// Open key presses: rawkey -> press time.
    open_keys: HashMap<u8, f64>,
    /// Open port-1 mouse-button presses, indexed like PORT1_BUTTONS.
    open_clicks: [Option<f64>; 3],
    /// Open port-2 control presses, indexed like PORT2_CONTROLS.
    open_joys: [Option<f64>; 11],
    /// Input state at the previous `observe`, the diff baseline. Controls
    /// already held when recording starts are not recorded.
    prev: Option<InputState>,
}

/// Format an event time truncated (not rounded) to milliseconds. Replay
/// fires an event at the first frame boundary at-or-after its timestamp,
/// so rounding a boundary time UP would push the event one frame late;
/// truncation keeps it at-or-just-before the boundary it was recorded on.
fn fmt_secs(secs: f64) -> String {
    format!("{:.3}", (secs * 1000.0).floor() / 1000.0)
}

fn hold_ms(press_secs: f64, release_secs: f64) -> u32 {
    // Floored for the same boundary-preserving reason as fmt_secs.
    (((release_secs - press_secs) * 1000.0).floor().max(0.0) as u32).max(MIN_HOLD_MS)
}

impl InputRecorder {
    pub fn new(now_secs: f64) -> Self {
        Self {
            last_secs: now_secs,
            lines: Vec::new(),
            open_keys: HashMap::new(),
            open_clicks: [None; 3],
            open_joys: [None; 11],
            prev: None,
        }
    }

    pub fn events_recorded(&self) -> usize {
        self.lines.len()
            + self.open_keys.len()
            + self.open_clicks.iter().flatten().count()
            + self.open_joys.iter().flatten().count()
    }

    /// Record an Amiga key transition that reached the keyboard queue
    /// (call from the single key choke point, after the reset chord has
    /// had its chance to consume the event).
    pub fn record_key(&mut self, rawkey: u8, pressed: bool, secs: f64) {
        if pressed {
            self.open_keys.entry(rawkey).or_insert(secs);
        } else if let Some(press) = self.open_keys.remove(&rawkey) {
            self.lines.push((
                press,
                format!(
                    "key-after {} 0x{rawkey:02X} {}",
                    fmt_secs(press),
                    hold_ms(press, secs)
                ),
            ));
        }
    }

    /// Record a floppy insert that succeeded.
    pub fn record_disk_insert(&mut self, drive_idx: usize, path: &Path, secs: f64) {
        let path = path.display().to_string();
        let path = if path.chars().any(char::is_whitespace) {
            format!("\"{path}\"")
        } else {
            path
        };
        self.lines.push((
            secs,
            format!("insert-disk-after {} df{drive_idx} {path}", fmt_secs(secs)),
        ));
    }

    /// Diff the live input state against the previous quantum: port-1
    /// mouse buttons and motion counters, and the port-2 joystick/pad.
    pub fn observe(&mut self, input: &InputState, secs: f64) {
        self.last_secs = secs;
        let Some(prev) = self.prev.replace(*input) else {
            return;
        };

        let dx = input.mouse_x_port1.wrapping_sub(prev.mouse_x_port1) as i8;
        let dy = input.mouse_y_port1.wrapping_sub(prev.mouse_y_port1) as i8;
        if dx != 0 || dy != 0 {
            self.lines
                .push((secs, format!("mouse-after {} {dx} {dy}", fmt_secs(secs))));
        }

        for (idx, (name, read)) in PORT1_BUTTONS.iter().enumerate() {
            match (read(&prev), read(input)) {
                (false, true) => self.open_clicks[idx] = Some(secs),
                (true, false) => {
                    if let Some(press) = self.open_clicks[idx].take() {
                        self.lines.push((
                            press,
                            format!(
                                "click-after {} {name} {}",
                                fmt_secs(press),
                                hold_ms(press, secs)
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }

        for (idx, (name, read)) in PORT2_CONTROLS.iter().enumerate() {
            match (read(&prev), read(input)) {
                (false, true) => self.open_joys[idx] = Some(secs),
                (true, false) => {
                    if let Some(press) = self.open_joys[idx].take() {
                        self.lines.push((
                            press,
                            format!(
                                "joy-after {} {name} {}",
                                fmt_secs(press),
                                hold_ms(press, secs)
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    /// Close any still-held controls at the last observed time and render
    /// the script, sorted by event time.
    pub fn finish(mut self) -> String {
        let end = self.last_secs;
        let open_keys: Vec<(u8, f64)> = self.open_keys.drain().collect();
        for (rawkey, press) in open_keys {
            self.lines.push((
                press,
                format!(
                    "key-after {} 0x{rawkey:02X} {}",
                    fmt_secs(press),
                    hold_ms(press, end)
                ),
            ));
        }
        for (idx, (name, _)) in PORT1_BUTTONS.iter().enumerate() {
            if let Some(press) = self.open_clicks[idx].take() {
                self.lines.push((
                    press,
                    format!(
                        "click-after {} {name} {}",
                        fmt_secs(press),
                        hold_ms(press, end)
                    ),
                ));
            }
        }
        for (idx, (name, _)) in PORT2_CONTROLS.iter().enumerate() {
            if let Some(press) = self.open_joys[idx].take() {
                self.lines.push((
                    press,
                    format!(
                        "joy-after {} {name} {}",
                        fmt_secs(press),
                        hold_ms(press, end)
                    ),
                ));
            }
        }
        self.lines
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = String::new();
        let _ = writeln!(out, "# Copperline input script (recorded live session)");
        let _ = writeln!(
            out,
            "# Replay: copperline --config <config> --script <this file>"
        );
        let _ = writeln!(out, "# Times are absolute emulated seconds.");
        for (_, line) in &self.lines {
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> InputState {
        InputState::default()
    }

    #[test]
    fn key_pairs_merge_into_key_after_holds() {
        let mut rec = InputRecorder::new(1.0);
        rec.record_key(0x45, true, 1.5);
        rec.record_key(0x45, false, 1.75);
        // Release without a recorded press is dropped, not paired backwards.
        rec.record_key(0x44, false, 1.8);
        let script = rec.finish();
        assert!(script.contains("key-after 1.500 0x45 250"), "{script}");
        assert!(!script.contains("0x44"), "{script}");
    }

    #[test]
    fn keys_still_held_at_stop_close_at_last_observed_time() {
        let mut rec = InputRecorder::new(0.0);
        rec.observe(&base_input(), 2.0);
        rec.record_key(0x60, true, 2.0);
        rec.observe(&base_input(), 3.0);
        let script = rec.finish();
        assert!(script.contains("key-after 2.000 0x60 1000"), "{script}");
    }

    #[test]
    fn mouse_motion_diffs_wrap_and_coalesce_per_observe() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        // Forward motion, including a wrap through 255 -> 2.
        input.mouse_x_port1 = 253;
        input.mouse_y_port1 = 10;
        rec.observe(&input, 1.02);
        input.mouse_x_port1 = 2;
        rec.observe(&input, 1.04);
        // No motion: no line.
        rec.observe(&input, 1.06);
        let script = rec.finish();
        assert!(script.contains("mouse-after 1.020 -3 10"), "{script}");
        assert!(script.contains("mouse-after 1.040 5 0"), "{script}");
        assert_eq!(script.matches("mouse-after").count(), 2, "{script}");
    }

    #[test]
    fn buttons_and_pad_controls_pair_across_observes() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 5.0);
        input.lmb_port1 = true;
        input.cd32_green_port2 = true;
        rec.observe(&input, 5.5);
        input.lmb_port1 = false;
        rec.observe(&input, 6.0);
        input.cd32_green_port2 = false;
        rec.observe(&input, 6.5);
        let script = rec.finish();
        assert!(script.contains("click-after 5.500 left 500"), "{script}");
        assert!(script.contains("joy-after 5.500 green 1000"), "{script}");
    }

    #[test]
    fn sub_quantum_pairs_get_a_minimum_hold() {
        let mut rec = InputRecorder::new(0.0);
        rec.record_key(0x35, true, 1.0);
        rec.record_key(0x35, false, 1.0);
        let script = rec.finish();
        assert!(script.contains("key-after 1.000 0x35 20"), "{script}");
    }

    #[test]
    fn disk_inserts_quote_paths_with_spaces() {
        let mut rec = InputRecorder::new(0.0);
        rec.record_disk_insert(0, Path::new("/tmp/plain.adf"), 3.0);
        rec.record_disk_insert(1, Path::new("/tmp/with space.adf"), 4.0);
        let script = rec.finish();
        assert!(
            script.contains("insert-disk-after 3.000 df0 /tmp/plain.adf"),
            "{script}"
        );
        assert!(
            script.contains("insert-disk-after 4.000 df1 \"/tmp/with space.adf\""),
            "{script}"
        );
    }

    #[test]
    fn output_is_sorted_by_event_time() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        rec.record_key(0x20, true, 9.0);
        rec.record_key(0x20, false, 9.5);
        input.mouse_x_port1 = 4;
        rec.observe(&input, 2.0);
        let script = rec.finish();
        let mouse_pos = script.find("mouse-after").unwrap();
        let key_pos = script.find("key-after").unwrap();
        assert!(mouse_pos < key_pos, "{script}");
    }
}
