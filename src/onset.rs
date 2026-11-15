//! Turns audio into an onset envelope: a low-rate signal that spikes wherever
//! something new starts to sound. Tempo estimation only ever looks at this
//! envelope, so audio can be discarded as it is decoded and memory use stays
//! flat however long the file is.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Target envelope sample rate. The true rate depends on how evenly this
/// divides the audio rate, see [`Envelope::frame_rate`].
const FRAME_RATE: f64 = 100.0;
const FRAME_SECS: f64 = 0.046;
const MEL_BANDS: usize = 40;
const MEL_MIN_HZ: f64 = 30.0;
const MEL_MAX_HZ: f64 = 8000.0;
/// Log compression strength, so quiet onsets count as well as loud ones.
const COMPRESSION: f32 = 1000.0;
/// Bands fade out above this. Beats are felt mostly in the low end, and summing
/// bands evenly lets a quiet hi-hat, which spans many high bands, match a kick
/// drum. That makes hats on the off-beat read as double time.
const ACCENT_CORNER_HZ: f64 = 500.0;
/// Compare against the frame this far back rather than the previous one,
/// because attacks spread over a few frames at this hop size.
const FLUX_LAG: usize = 3;
/// Width of the moving average removed from the flux, to flatten out
/// sustained noisy passages.
const BASELINE_SECS: f64 = 1.0;

#[derive(Debug, Clone)]
pub struct Envelope {
    pub values: Vec<f32>,
    pub frame_rate: f64,
    pub duration_secs: f64,
}

/// One triangular mel filter, stored sparsely.
struct Band {
    first_bin: usize,
    weights: Vec<f32>,
    /// How much this band's flux counts towards the envelope.
    accent: f32,
}

pub struct OnsetDetector {
    sample_rate: f64,
    hop: usize,
    window: Vec<f32>,
    gain: f32,
    bands: Vec<Band>,
    fft: Arc<dyn RealToComplex<f32>>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    pending: Vec<f32>,
    pending_start: usize,
    history: Vec<Vec<f32>>,
    history_pos: usize,
    frames_seen: usize,
    samples_seen: u64,
    flux: Vec<f32>,
}

impl OnsetDetector {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate);
        let hop = ((rate / FRAME_RATE).round() as usize).max(1);
        let size = ((rate * FRAME_SECS) as usize).max(16).next_power_of_two();

        let window: Vec<f32> = (0..size)
            .map(|i| {
                let phase = std::f64::consts::TAU * i as f64 / size as f64;
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        // Scale so a full-scale sine has a spectral magnitude of about 1.
        let gain = 2.0 / window.iter().sum::<f32>();

        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let frame = fft.make_input_vec();
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let bands = mel_filterbank(rate, size);

        Self {
            sample_rate: rate,
            hop,
            window,
            gain,
            history: vec![vec![0.0; bands.len()]; FLUX_LAG],
            bands,
            fft,
            frame,
            spectrum,
            scratch,
            pending: Vec::new(),
            pending_start: 0,
            history_pos: 0,
            frames_seen: 0,
            samples_seen: 0,
            flux: Vec::new(),
        }
    }

    /// Feed the next run of mono samples, nominally in `-1.0..=1.0`.
    pub fn push(&mut self, samples: &[f32]) {
        self.samples_seen += samples.len() as u64;
        self.pending.extend_from_slice(samples);
        while self.pending.len() - self.pending_start >= self.frame.len() {
            self.process_frame();
            self.pending_start += self.hop;
        }
        self.pending.drain(..self.pending_start);
        self.pending_start = 0;
    }

    fn process_frame(&mut self) {
        let input = &self.pending[self.pending_start..][..self.frame.len()];
        for ((out, sample), window) in self.frame.iter_mut().zip(input).zip(&self.window) {
            *out = sample * window;
        }
        self.fft
            .process_with_scratch(&mut self.frame, &mut self.spectrum, &mut self.scratch)
            .expect("buffers come from the planner");

        // The oldest slot is exactly FLUX_LAG frames back, so read it before
        // overwriting it with this frame.
        let previous = &mut self.history[self.history_pos];
        let mut flux = 0.0;
        for (band, previous) in self.bands.iter().zip(previous.iter_mut()) {
            let bins = &self.spectrum[band.first_bin..][..band.weights.len()];
            let energy: f32 = bins
                .iter()
                .zip(&band.weights)
                .map(|(b, w)| b.norm() * w)
                .sum();
            let level = (1.0 + COMPRESSION * self.gain * energy).ln();
            if self.frames_seen >= FLUX_LAG {
                flux += band.accent * (level - *previous).max(0.0);
            }
            *previous = level;
        }
        self.flux.push(flux / self.bands.len() as f32);
        self.history_pos = (self.history_pos + 1) % FLUX_LAG;
        self.frames_seen += 1;
    }

    pub fn finish(self) -> Envelope {
        let frame_rate = self.sample_rate / self.hop as f64;
        Envelope {
            values: remove_baseline(&self.flux, (BASELINE_SECS * frame_rate) as usize / 2),
            frame_rate,
            duration_secs: self.samples_seen as f64 / self.sample_rate,
        }
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10f64.powf(mel / 2595.0) - 1.0)
}

fn mel_filterbank(sample_rate: f64, fft_size: usize) -> Vec<Band> {
    let bin_hz = sample_rate / fft_size as f64;
    let top_bin = fft_size / 2;
    let low = hz_to_mel(MEL_MIN_HZ);
    let high = hz_to_mel(MEL_MAX_HZ.min(sample_rate / 2.0));
    let edge = |i: usize| mel_to_hz(low + (high - low) * i as f64 / (MEL_BANDS + 1) as f64);

    (0..MEL_BANDS)
        .map(|i| {
            let (left, centre, right) = (edge(i), edge(i + 1), edge(i + 2));
            let first_bin = ((left / bin_hz).ceil() as usize).min(top_bin);
            let last_bin = ((right / bin_hz).floor() as usize).clamp(first_bin, top_bin);
            let weights = (first_bin..=last_bin)
                .map(|bin| {
                    let hz = bin as f64 * bin_hz;
                    let rising = (hz - left) / (centre - left);
                    let falling = (right - hz) / (right - centre);
                    rising.min(falling).max(0.0) as f32
                })
                .collect();
            Band {
                first_bin,
                weights,
                accent: (1.0 / (1.0 + centre / ACCENT_CORNER_HZ)) as f32,
            }
        })
        .collect()
}

/// Subtract a centred moving average and clamp what falls below it to zero.
fn remove_baseline(flux: &[f32], radius: usize) -> Vec<f32> {
    let mut sum = 0.0f64;
    let prefix: Vec<f64> = std::iter::once(0.0)
        .chain(flux.iter().map(|&v| {
            sum += f64::from(v);
            sum
        }))
        .collect();
    (0..flux.len())
        .map(|i| {
            let start = i.saturating_sub(radius);
            let end = (i + radius + 1).min(flux.len());
            let mean = (prefix[end] - prefix[start]) / (end - start) as f64;
            (flux[i] - mean as f32).max(0.0)
        })
        .collect()
}
