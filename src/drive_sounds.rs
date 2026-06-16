// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthesized 3.5" floppy drive sound effects.
//!
//! Nothing here is sampled from real hardware (or borrowed from other
//! emulators); the motor, seek and read noises are generated from
//! scratch with a handful of cheap oscillator/noise/filter voices that
//! aim for the character of an Amiga DD mechanism:
//!
//! - Motor: a low brushless-spindle hum (two slightly inharmonic
//!   partials) over lowpassed noise rumble, amplitude-wobbled once per
//!   platter revolution. Pitch and level glide with the spin level the
//!   floppy model reports, so spin-up/spin-down sweep naturally.
//! - Seek/step: each head-step pulse fires a short damped "tick" (a
//!   snappy lowpassed noise transient plus a mid-frequency damped
//!   sine body) with small random pitch/level variation. Trackdisk's
//!   slow no-disk polling produces the classic lone click; a fast
//!   multi-track seek overlaps clicks every ~3 ms into the familiar
//!   buzz.
//! - Read/write: a faint band-limited hiss while disk DMA is moving
//!   data, gated by the platter actually spinning.
//!
//! The generator runs at the host mixer rate and is mixed into Paula's
//! stereo output after the LED filter (the drive is an acoustic source
//! next to the machine, not part of Paula's filtered audio path). When
//! idle it emits exact 0.0 so the live sink's leading-silence skip
//! keeps working.

use crate::audio::MIX_SAMPLE_RATE;

pub const NUM_DRIVES: usize = 4;
const NUM_CLICK_VOICES: usize = 8;

// Master gain staging, relative to Paula's roughly [-1, 1] music mix.
// The drive should read as a mechanism in the background, not an
// instrument: clicks peak around 0.2, the motor sits well below that.
const CLICK_GAIN: f32 = 0.20;
const MOTOR_HUM_GAIN: f32 = 0.030;
const MOTOR_RUMBLE_GAIN: f32 = 0.045;
const READ_HISS_GAIN: f32 = 0.020;

// Motor hum partials. A 300 RPM spindle with a multi-pole brushless
// motor hums around mains-ish frequencies; the second partial is
// deliberately inharmonic so the result beats slightly like a real
// motor instead of sounding like an organ tone.
const MOTOR_HUM_BASE_HZ: f32 = 58.0;
const MOTOR_HUM_SECOND_RATIO: f32 = 2.13;
// The platter turns at 5 Hz (300 RPM); the rumble level wobbles once
// per revolution.
const MOTOR_WOBBLE_HZ: f32 = 5.0;
const MOTOR_WOBBLE_DEPTH: f32 = 0.25;
// Rumble noise lowpass sweeps up as the platter comes to speed.
const MOTOR_RUMBLE_LP_MIN_HZ: f32 = 140.0;
const MOTOR_RUMBLE_LP_MAX_HZ: f32 = 420.0;
// Smooths externally supplied spin levels (updated at chipset-tick
// granularity) to avoid zipper noise.
const MOTOR_SPIN_SMOOTH_HZ: f32 = 18.0;

// Step click envelope time constants.
const CLICK_SNAP_DECAY_S: f32 = 0.0028;
const CLICK_BODY_DECAY_S: f32 = 0.013;
const CLICK_BODY_MIN_HZ: f32 = 460.0;
const CLICK_BODY_HZ_SPREAD: f32 = 140.0;
const CLICK_SNAP_LP_HZ: f32 = 3200.0;
// A voice this quiet is retired.
const CLICK_DEAD_LEVEL: f32 = 1.0e-4;

// Read hiss band and gating envelope.
const READ_HISS_LP_HZ: f32 = 3600.0;
const READ_HISS_HP_HZ: f32 = 900.0;
const READ_ATTACK_S: f32 = 0.006;
const READ_RELEASE_S: f32 = 0.070;

const SILENT_LEVEL: f32 = 1.0e-4;
const TWO_PI: f32 = std::f32::consts::TAU;

/// Advance a sine phase accumulator and keep it in `[0, TWO_PI)`. Every
/// increment here is a small fraction of a cycle per host sample, so wrapping
/// is a single subtraction rather than a floating-point modulo -- `% TWO_PI`
/// lowered to `fmodf`, which showed up as several percent of the whole
/// emulator on the per-sample audio path. The loop covers the (rare) case of a
/// click body pitch high enough to advance more than a full cycle per sample.
#[inline]
fn wrap_phase(phase: f32, inc: f32) -> f32 {
    let mut next = phase + inc;
    while next >= TWO_PI {
        next -= TWO_PI;
    }
    next
}

/// One-pole lowpass coefficient for a cutoff at the mixer rate. The
/// linear approximation is fine for the sub-4 kHz cutoffs used here.
fn lowpass_coeff(cutoff_hz: f32) -> f32 {
    (TWO_PI * cutoff_hz / MIX_SAMPLE_RATE as f32).min(1.0)
}

/// Per-sample decay multiplier for an exponential envelope with the
/// given time constant.
fn decay_per_sample(time_constant_s: f32) -> f32 {
    (-1.0 / (time_constant_s * MIX_SAMPLE_RATE as f32)).exp()
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct MotorVoice {
    /// Smoothed spin level actually driving the synthesis, 0..1.
    spin: f32,
    /// Spin level last reported by the floppy model.
    spin_target: f32,
    hum_phase: f32,
    wobble_phase: f32,
    rumble_lp: f32,
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct ClickVoice {
    /// Envelope of the initial noise snap.
    snap_env: f32,
    /// Envelope of the damped-sine body.
    body_env: f32,
    body_phase: f32,
    body_phase_inc: f32,
    snap_lp: f32,
    amp: f32,
}

impl ClickVoice {
    fn active(&self) -> bool {
        self.body_env > CLICK_DEAD_LEVEL || self.snap_env > CLICK_DEAD_LEVEL
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct DriveSounds {
    enabled: bool,
    volume: f32,
    motors: [MotorVoice; NUM_DRIVES],
    clicks: [ClickVoice; NUM_CLICK_VOICES],
    next_click_voice: usize,
    read_level: f32,
    read_active: bool,
    read_lp: f32,
    read_hp_lp: f32,
    rng: u32,
}

impl Default for DriveSounds {
    fn default() -> Self {
        Self::new()
    }
}

impl DriveSounds {
    pub fn new() -> Self {
        Self {
            enabled: true,
            volume: 1.0,
            motors: [MotorVoice::default(); NUM_DRIVES],
            clicks: [ClickVoice::default(); NUM_CLICK_VOICES],
            next_click_voice: 0,
            read_level: 0.0,
            read_active: false,
            read_lp: 0.0,
            read_hp_lp: 0.0,
            rng: 0x2F6E_2B1D,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            // Drop in-flight voices so a later re-enable does not
            // replay the tail of old activity.
            self.clicks = [ClickVoice::default(); NUM_CLICK_VOICES];
            self.read_level = 0.0;
            for motor in &mut self.motors {
                motor.spin = 0.0;
            }
        }
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_volume_percent(&mut self, percent: u8) {
        self.volume = f32::from(percent.min(100)) / 100.0;
    }

    /// Report a drive's platter spin level, 0.0 (stopped) to 1.0 (at
    /// speed). The floppy model's motor accumulator already ramps this
    /// over the real ~0.5 s spin-up/spin-down, which gives the hum its
    /// pitch/level glide.
    pub fn set_motor_spin(&mut self, drive: usize, spin: f32) {
        if let Some(motor) = self.motors.get_mut(drive) {
            motor.spin_target = spin.clamp(0.0, 1.0);
        }
    }

    /// Gate the read/write hiss on disk DMA actually moving data.
    pub fn set_read_active(&mut self, active: bool) {
        self.read_active = active;
    }

    /// Fire one head-step click. Rapid bursts (seeks) overlap voices
    /// into the characteristic seek buzz.
    pub fn step_pulse(&mut self) {
        if !self.enabled {
            return;
        }
        let voice = &mut self.clicks[self.next_click_voice];
        self.next_click_voice = (self.next_click_voice + 1) % NUM_CLICK_VOICES;
        // Small random spread keeps a seek from sounding machine-gun
        // perfect; real steppers never hit twice identically.
        let amp_jitter = 0.80 + 0.20 * next_noise_unipolar(&mut self.rng);
        let body_hz = CLICK_BODY_MIN_HZ + CLICK_BODY_HZ_SPREAD * next_noise_unipolar(&mut self.rng);
        voice.snap_env = 1.0;
        voice.body_env = 1.0;
        voice.body_phase = 0.0;
        voice.body_phase_inc = TWO_PI * body_hz / MIX_SAMPLE_RATE as f32;
        voice.snap_lp = 0.0;
        voice.amp = CLICK_GAIN * amp_jitter;
    }

    /// Render the next mono sample. Exact 0.0 while idle.
    pub fn next_sample(&mut self) -> f32 {
        if !self.enabled || self.volume <= 0.0 {
            return 0.0;
        }
        if self.is_idle() {
            return 0.0;
        }

        let mut sample = 0.0f32;
        let spin_smooth = lowpass_coeff(MOTOR_SPIN_SMOOTH_HZ);
        let mut max_spin = 0.0f32;
        for motor in &mut self.motors {
            motor.spin += spin_smooth * (motor.spin_target - motor.spin);
            if motor.spin < SILENT_LEVEL && motor.spin_target == 0.0 {
                motor.spin = 0.0;
                continue;
            }
            max_spin = max_spin.max(motor.spin);

            // Hum pitch glides up with spin so the motor audibly winds
            // up rather than fading in at full pitch.
            let pitch = 0.35 + 0.65 * motor.spin;
            let hum_inc = TWO_PI * MOTOR_HUM_BASE_HZ * pitch / MIX_SAMPLE_RATE as f32;
            motor.hum_phase = wrap_phase(motor.hum_phase, hum_inc);
            let hum =
                motor.hum_phase.sin() + 0.55 * (motor.hum_phase * MOTOR_HUM_SECOND_RATIO).sin();

            let wobble_inc = TWO_PI * MOTOR_WOBBLE_HZ * motor.spin / MIX_SAMPLE_RATE as f32;
            motor.wobble_phase = wrap_phase(motor.wobble_phase, wobble_inc);
            let wobble = 1.0 + MOTOR_WOBBLE_DEPTH * motor.wobble_phase.sin();

            let rumble_cutoff = MOTOR_RUMBLE_LP_MIN_HZ
                + (MOTOR_RUMBLE_LP_MAX_HZ - MOTOR_RUMBLE_LP_MIN_HZ) * motor.spin;
            let noise = next_noise_bipolar(&mut self.rng);
            motor.rumble_lp += lowpass_coeff(rumble_cutoff) * (noise - motor.rumble_lp);

            // Level rises faster than linearly with spin so a barely
            // turning platter is barely audible.
            let level = motor.spin * motor.spin.sqrt();
            sample += level * (MOTOR_HUM_GAIN * hum + MOTOR_RUMBLE_GAIN * wobble * motor.rumble_lp);
        }

        let snap_decay = decay_per_sample(CLICK_SNAP_DECAY_S);
        let body_decay = decay_per_sample(CLICK_BODY_DECAY_S);
        let snap_lp_coeff = lowpass_coeff(CLICK_SNAP_LP_HZ);
        for voice in &mut self.clicks {
            if !voice.active() {
                continue;
            }
            let noise = next_noise_bipolar(&mut self.rng);
            voice.snap_lp += snap_lp_coeff * (noise - voice.snap_lp);
            voice.body_phase = wrap_phase(voice.body_phase, voice.body_phase_inc);
            let body = voice.body_phase.sin() * voice.body_env;
            sample += voice.amp * (1.1 * voice.snap_lp * voice.snap_env + 0.55 * body);
            voice.snap_env *= snap_decay;
            voice.body_env *= body_decay;
        }

        // Read hiss: only audible while the platter is actually
        // spinning under the head.
        let read_target = if self.read_active && max_spin > 0.5 {
            1.0
        } else {
            0.0
        };
        let read_coeff = if read_target > self.read_level {
            lowpass_coeff(1.0 / (TWO_PI * READ_ATTACK_S))
        } else {
            lowpass_coeff(1.0 / (TWO_PI * READ_RELEASE_S))
        };
        self.read_level += read_coeff * (read_target - self.read_level);
        if self.read_level < SILENT_LEVEL && read_target == 0.0 {
            self.read_level = 0.0;
        }
        if self.read_level > 0.0 {
            let noise = next_noise_bipolar(&mut self.rng);
            self.read_lp += lowpass_coeff(READ_HISS_LP_HZ) * (noise - self.read_lp);
            self.read_hp_lp += lowpass_coeff(READ_HISS_HP_HZ) * (self.read_lp - self.read_hp_lp);
            let band = self.read_lp - self.read_hp_lp;
            sample += READ_HISS_GAIN * self.read_level * band;
        }

        sample * self.volume
    }

    fn is_idle(&self) -> bool {
        self.read_level == 0.0
            && !self.read_active
            && self
                .motors
                .iter()
                .all(|m| m.spin == 0.0 && m.spin_target == 0.0)
            && self.clicks.iter().all(|v| !v.active())
    }
}

/// xorshift32; deterministic, no host entropy.
fn next_rng(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform noise in [-1, 1).
fn next_noise_bipolar(state: &mut u32) -> f32 {
    (next_rng(state) as f32 / (u32::MAX as f32 / 2.0)) - 1.0
}

/// Uniform noise in [0, 1).
fn next_noise_unipolar(state: &mut u32) -> f32 {
    next_rng(state) as f32 / u32::MAX as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(sounds: &mut DriveSounds, samples: usize) -> Vec<f32> {
        (0..samples).map(|_| sounds.next_sample()).collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }

    #[test]
    fn idle_drive_sounds_emit_exact_silence() {
        let mut sounds = DriveSounds::new();
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn step_pulse_clicks_then_decays_back_to_silence() {
        let mut sounds = DriveSounds::new();
        sounds.step_pulse();
        let click = render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        assert!(peak(&click) > 0.01, "step produced no click");
        // After 200 ms the click is over and the output is bit-exact 0.
        let tail = render(&mut sounds, 1024);
        assert!(tail.iter().all(|&s| s == 0.0), "click did not fully decay");
    }

    #[test]
    fn motor_spin_produces_hum_and_spindown_returns_to_silence() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        let hum = render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        assert!(peak(&hum) > 0.005, "motor produced no hum");
        sounds.set_motor_spin(0, 0.0);
        // Let the smoothed level decay, then expect exact silence.
        render(&mut sounds, MIX_SAMPLE_RATE as usize);
        let tail = render(&mut sounds, 1024);
        assert!(tail.iter().all(|&s| s == 0.0), "motor did not spin down");
    }

    #[test]
    fn read_hiss_requires_a_spinning_platter() {
        let mut sounds = DriveSounds::new();
        sounds.set_read_active(true);
        let stopped = render(&mut sounds, 4096);
        assert!(
            peak(&stopped) < 1.0e-3,
            "read hiss audible with platter stopped"
        );

        // The hiss lives well above the motor bed in frequency, so
        // compare first-difference (high-frequency) energy with the
        // motor running, with and without a read in progress.
        let hf_energy =
            |samples: &[f32]| -> f32 { samples.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum() };
        sounds.set_motor_spin(0, 1.0);
        render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        sounds.set_read_active(false);
        let quiet_ref = hf_energy(&render(&mut sounds, 8192));
        sounds.set_read_active(true);
        render(&mut sounds, 4096); // let the attack envelope open
        let hissing = hf_energy(&render(&mut sounds, 8192));
        assert!(
            hissing > quiet_ref * 2.0,
            "read hiss added no high-frequency energy (quiet={quiet_ref}, hiss={hissing})"
        );
    }

    #[test]
    fn disabled_drive_sounds_are_silent_and_drop_pending_voices() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        sounds.step_pulse();
        sounds.set_read_active(true);
        sounds.set_enabled(false);
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
        // Re-enabling does not replay the old click; motor spin target
        // still applies, so silence requires the motor off too.
        sounds.set_motor_spin(0, 0.0);
        sounds.set_read_active(false);
        sounds.set_enabled(true);
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn volume_percent_scales_output() {
        let mut loud = DriveSounds::new();
        loud.set_volume_percent(100);
        loud.step_pulse();
        let mut quiet = DriveSounds::new();
        quiet.set_volume_percent(25);
        quiet.step_pulse();
        // Same deterministic seed, same click; only the volume differs.
        let loud_peak = peak(&render(&mut loud, 2048));
        let quiet_peak = peak(&render(&mut quiet, 2048));
        assert!(loud_peak > 0.0);
        assert!((quiet_peak / loud_peak - 0.25).abs() < 0.01);
    }

    #[test]
    fn seek_burst_stays_within_headroom() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        // 80-track seek at ~3 ms per step, all voices overlapping.
        let step_interval = (MIX_SAMPLE_RATE as usize * 3) / 1000;
        let mut rendered = Vec::new();
        for _ in 0..80 {
            sounds.step_pulse();
            rendered.extend(render(&mut sounds, step_interval));
        }
        assert!(peak(&rendered) < 1.0, "seek burst clips");
    }
}
