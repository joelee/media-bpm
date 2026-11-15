//! Synthesises audio with a known tempo.
#![allow(dead_code)]

use std::f32::consts::TAU;

use media_bpm::onset::{Envelope, OnsetDetector};

pub const RATE: u32 = 44_100;

/// A small xorshift generator, so test audio is the same on every run.
pub struct Noise(u32);

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

fn add(out: &mut [f32], start: usize, len: usize, mut sample: impl FnMut(usize) -> f32) {
    for (i, slot) in out.iter_mut().skip(start).take(len).enumerate() {
        *slot += sample(i);
    }
}

pub fn kick(out: &mut [f32], at: usize) {
    add(out, at, RATE as usize / 5, |i| {
        let t = i as f32 / RATE as f32;
        let phase = TAU * (50.0 * t + 8.0 * (1.0 - (-t * 30.0).exp()));
        0.9 * phase.sin() * (-t * 12.0).exp()
    });
}

pub fn snare(out: &mut [f32], at: usize, noise: &mut Noise) {
    add(out, at, RATE as usize / 6, |i| {
        let t = i as f32 / RATE as f32;
        let tone = (TAU * 190.0 * t).sin() * (-t * 25.0).exp();
        0.4 * tone + 0.45 * noise.next() * (-t * 18.0).exp()
    });
}

pub fn hat(out: &mut [f32], at: usize, noise: &mut Noise) {
    let mut last = 0.0;
    add(out, at, RATE as usize / 25, |i| {
        let t = i as f32 / RATE as f32;
        let white = noise.next();
        let high = white - last;
        last = white;
        0.15 * high * (-t * 90.0).exp()
    });
}

fn pluck(out: &mut [f32], at: usize, hz: f32) {
    add(out, at, RATE as usize / 8, |i| {
        let t = i as f32 / RATE as f32;
        let tone = (TAU * hz * t).sin() + 0.5 * (TAU * 2.0 * hz * t).sin();
        0.12 * tone * (-t * 20.0).exp()
    });
}

fn bass(out: &mut [f32], at: usize, len: usize, hz: f32) {
    add(out, at, len, |i| {
        let t = i as f32 / RATE as f32;
        0.25 * (TAU * hz * t).sin() * (-t * 3.0).exp()
    });
}

/// How a bar of four beats is played.
#[derive(Clone, Copy, PartialEq)]
pub enum Pattern {
    /// Evenly spaced clicks and nothing else.
    Clicks,
    /// Kick on every beat, the staple of house and techno.
    FourOnFloor,
    /// Kick on 1 and 3, snare on 2 and 4, eighth-note hats and a bass line.
    Rock,
    /// Kick on every beat, clap on 2 and 4, and a sixteenth-note synth line.
    Dance,
    /// Rock with swung hats, the second eighth pushed late, and sloppy timing.
    Shuffle,
}

/// Renders `pattern` for `seconds`, with the tempo at any moment given by
/// `bpm_at`. Each beat is placed by stepping along the curve.
pub fn render(pattern: Pattern, seconds: f64, bpm_at: impl Fn(f64) -> f64) -> Vec<f32> {
    let mut out = vec![0.0; (seconds * f64::from(RATE)) as usize];
    let mut noise = Noise::new(0x9E37_79B9);
    let notes = [55.0, 55.0, 65.4, 49.0];
    let mut time = 0.0;
    let mut eighth = 0usize;
    while time < seconds {
        let at = (time * f64::from(RATE)) as usize;
        let beat = eighth / 2;
        let step = 30.0 / bpm_at(time);
        let on_beat = eighth.is_multiple_of(2);
        match pattern {
            Pattern::Clicks if on_beat => kick(&mut out, at),
            Pattern::Clicks => {}
            Pattern::FourOnFloor => {
                if on_beat {
                    kick(&mut out, at);
                } else {
                    hat(&mut out, at, &mut noise);
                }
            }
            Pattern::Rock => {
                if on_beat && beat.is_multiple_of(2) {
                    kick(&mut out, at);
                    let len = (step * 2.0 * f64::from(RATE)) as usize;
                    bass(&mut out, at, len, notes[beat / 2 % notes.len()]);
                }
                if on_beat && beat % 2 == 1 {
                    snare(&mut out, at, &mut noise);
                }
                hat(&mut out, at, &mut noise);
            }
            Pattern::Dance => {
                if on_beat {
                    kick(&mut out, at);
                    if beat % 2 == 1 {
                        snare(&mut out, at, &mut noise);
                    }
                }
                let arp = [220.0, 330.0, 440.0, 660.0, 330.0, 494.0, 440.0, 330.0];
                let sixteenth = (step / 2.0 * f64::from(RATE)) as usize;
                pluck(&mut out, at, arp[eighth * 2 % arp.len()]);
                pluck(&mut out, at + sixteenth, arp[(eighth * 2 + 1) % arp.len()]);
            }
            Pattern::Shuffle => {
                let swing = if on_beat { 0.0 } else { step / 3.0 };
                let late = noise.next() as f64 * 0.008 + 0.008;
                let at = ((time + swing + late) * f64::from(RATE)) as usize;
                if on_beat && beat.is_multiple_of(2) {
                    kick(&mut out, at);
                }
                if on_beat && beat % 2 == 1 {
                    snare(&mut out, at, &mut noise);
                }
                hat(&mut out, at, &mut noise);
            }
        }
        time += step;
        eighth += 1;
    }
    out
}

/// Irregular voiced bursts and pauses with the rhythm of speech but no beat.
pub fn speech(seconds: usize, seed: u32, level: f32) -> Vec<f32> {
    let mut noise = Noise::new(seed);
    let mut timing = Noise::new(seed.wrapping_mul(31));
    let mut out = vec![0.0; seconds * RATE as usize];
    let mut at = 0;
    while at < out.len() {
        let len = ((0.08 + 0.1 * (timing.next() + 1.0)) * RATE as f32) as usize;
        let pitch = 100.0 + 60.0 * timing.next();
        add(&mut out, at, len, |i| {
            let t = i as f32 / RATE as f32;
            let shape = (std::f32::consts::PI * i as f32 / len as f32).sin();
            level * shape * ((TAU * pitch * t).sin() + 0.3 * noise.next())
        });
        let pause = 0.03 + 0.25 * ((timing.next() + 1.0) / 2.0).powi(3);
        at += len + (pause * RATE as f32) as usize;
    }
    out
}

pub fn envelope(samples: &[f32]) -> Envelope {
    let mut detector = OnsetDetector::new(RATE);
    // Fed in irregular chunks, the way a decoder delivers audio.
    for chunk in samples.chunks(1152) {
        detector.push(chunk);
    }
    detector.finish()
}

/// Encodes mono samples as a 16-bit PCM WAV file.
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}
