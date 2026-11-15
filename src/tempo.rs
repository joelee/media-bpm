//! Estimates a tempo curve from an onset envelope.
//!
//! The envelope is cut into overlapping windows. Each window scores every
//! candidate tempo, and a Viterbi search then picks the path through time with
//! the best total score. Without that step a single window misread at half or
//! double time would become the reported peak.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::onset::Envelope;

const HOP_SECS: f64 = 1.0;
/// Below this there are too few beats to call anything a tempo.
const MIN_DURATION_SECS: f64 = 5.0;
/// Ratio between neighbouring candidate tempos.
const GRID_STEP: f64 = 1.005;
/// Autocorrelation gets noisy as the overlap shrinks, so stop at this fraction
/// of the window.
const MAX_LAG_FRACTION: f64 = 2.0 / 3.0;
const MAX_COMB_TEETH: usize = 16;
/// Listeners favour tempos around this, which breaks the tie between readings
/// an octave apart. The width is in octaves.
const PRIOR_CENTRE_BPM: f64 = 120.0;
const PRIOR_WIDTH_OCTAVES: f64 = 0.6;
/// A tempo needs at least this share of the window's strongest spectral peak to
/// escape a penalty.
const SPECTRAL_FLOOR: f32 = 0.25;
/// Ratios a tempo reading can slip by without the music changing, because they
/// land on another level of the same pulse.
const METRICAL_RATIOS: [f64; 8] = [2.0, 0.5, 3.0, 1.0 / 3.0, 1.5, 2.0 / 3.0, 4.0 / 3.0, 0.75];
/// Share of the beat windows that must agree before a track has a main tempo.
const MIN_DOMINANT_SHARE: f64 = 0.4;
/// How close two tempos must be to count as the same one.
const SAME_TEMPO: f64 = 0.03;
/// Viterbi cost per grid step of tempo change between windows.
const SLOPE_COST: f32 = 0.02;
/// Viterbi cost of an arbitrary jump, which is what a real tempo change takes.
const JUMP_COST: f32 = 1.0;
/// Radius of the median filter that removes estimator jitter from the curve.
const SMOOTHING_RADIUS: usize = 2;
/// Windows whose chosen tempo correlates worse than this have no real beat.
const MIN_CONFIDENCE: f32 = 0.1;
/// Speech and other irregular sound can line up with some tempo for a moment,
/// which confidence alone cannot catch without also rejecting music under
/// dialogue. A beat is what keeps going, so require it to last this long.
const MIN_RUN_SECS: f64 = 8.0;
/// Windows with less onset activity than this (RMS of the envelope) hold no
/// onsets at all. Normalising would turn the faint ripple of a steady tone into
/// a perfect rhythm. Quiet music stays above it thanks to log compression.
const MIN_ACTIVITY: f32 = 0.003;

#[derive(Debug, Clone)]
pub struct Options {
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// Length of the sliding window. Shorter windows react faster to tempo
    /// changes but give noisier estimates.
    pub window_secs: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_bpm: 40.0,
            max_bpm: 240.0,
            window_secs: 10.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    /// Mean tempo over the parts of the file that have a beat.
    pub average_bpm: f64,
    /// Highest tempo held over one window.
    pub peak_bpm: f64,
    /// Where the peak's window is centred.
    pub peak_at_secs: f64,
    pub duration_secs: f64,
    /// Fraction of the file, from 0 to 1, in which a beat was found.
    pub coverage: f64,
}

pub fn estimate(envelope: &Envelope, options: &Options) -> Option<Report> {
    let fps = envelope.frame_rate;
    let values = &envelope.values;
    if (values.len() as f64) < MIN_DURATION_SECS * fps {
        return None;
    }

    let window = ((options.window_secs * fps) as usize).min(values.len());
    let hop = (HOP_SECS * fps) as usize;
    let grid = tempo_grid(options.min_bpm, options.max_bpm);
    let priors: Vec<f32> = grid.iter().map(|&bpm| prior(bpm)).collect();
    let mut analyzer = WindowAnalyzer::new(window, fps);

    let starts: Vec<usize> = (0..)
        .map(|i| i * hop)
        .take_while(|s| s + window <= values.len())
        .collect();
    let mut confidence = vec![0.0; starts.len() * grid.len()];
    let mut emission = vec![0.0; starts.len() * grid.len()];
    for (&start, (confidence, emission)) in starts.iter().zip(
        confidence
            .chunks_exact_mut(grid.len())
            .zip(emission.chunks_exact_mut(grid.len())),
    ) {
        analyzer.score(
            &values[start..start + window],
            &grid,
            &priors,
            confidence,
            emission,
        );
    }

    let path = best_path(&emission, grid.len());
    let curve: Vec<f64> = path
        .iter()
        .enumerate()
        .map(|(t, &bin)| refine(&grid, &emission[t * grid.len()..][..grid.len()], bin))
        .collect();
    let curve = median_filter(&curve, SMOOTHING_RADIUS);

    let confident: Vec<bool> = (0..curve.len())
        .map(|t| confidence[t * grid.len() + path[t]] >= MIN_CONFIDENCE)
        .collect();
    let min_run = ((MIN_RUN_SECS / HOP_SECS) as usize).min(curve.len());
    let beats = sustained(&confident, min_run);
    let mut curve = curve;
    fold_to_dominant(&mut curve, &beats);
    let peak = *beats
        .iter()
        .max_by(|&&a, &&b| curve[a].total_cmp(&curve[b]))?;
    Some(Report {
        average_bpm: beats.iter().map(|&t| curve[t]).sum::<f64>() / beats.len() as f64,
        peak_bpm: curve[peak],
        peak_at_secs: (starts[peak] + window / 2) as f64 / fps,
        duration_secs: envelope.duration_secs,
        coverage: beats.len() as f64 / curve.len() as f64,
    })
}

fn tempo_grid(min_bpm: f64, max_bpm: f64) -> Vec<f64> {
    std::iter::successors(Some(min_bpm), |bpm| Some(bpm * GRID_STEP))
        .take_while(|&bpm| bpm <= max_bpm)
        .collect()
}

fn prior(bpm: f64) -> f32 {
    let octaves = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_WIDTH_OCTAVES;
    (-0.5 * octaves * octaves).exp() as f32
}

/// Scores candidate tempos for one window of the envelope.
struct WindowAnalyzer {
    fps: f64,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    power: Vec<f32>,
    strength: Vec<f32>,
    autocorrelation: Vec<f32>,
    max_lag: usize,
    size: usize,
}

impl WindowAnalyzer {
    fn new(window: usize, fps: f64) -> Self {
        // Doubled so the circular correlation of the FFT cannot wrap around.
        let size = (window * 2).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        Self {
            fps,
            time: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            power: Vec::new(),
            strength: Vec::new(),
            autocorrelation: vec![0.0; size],
            max_lag: ((window as f64 * MAX_LAG_FRACTION) as usize).max(1),
            size,
            forward,
            inverse,
        }
    }

    /// Fill `confidence` with how well each tempo fits the window (-1 to 1)
    /// and `emission` with the score the path search should use for it.
    fn score(
        &mut self,
        window: &[f32],
        grid: &[f64],
        priors: &[f32],
        confidence: &mut [f32],
        emission: &mut [f32],
    ) {
        let mean = window.iter().sum::<f32>() / window.len() as f32;
        self.time.fill(0.0);
        for (out, value) in self.time.iter_mut().zip(window) {
            *out = value - mean;
        }
        self.forward
            .process(&mut self.time, &mut self.spectrum)
            .expect("buffers fit the plan");
        self.power.clear();
        self.power
            .extend(self.spectrum.iter().map(|bin| bin.norm_sqr()));
        for (bin, &power) in self.spectrum.iter_mut().zip(&self.power) {
            *bin = Complex::new(power, 0.0);
        }
        self.inverse
            .process(&mut self.spectrum, &mut self.autocorrelation)
            .expect("buffers fit the plan");

        let energy = self.autocorrelation[0];
        let activity = (energy / self.size as f32 / window.len() as f32).sqrt();
        if activity < MIN_ACTIVITY {
            return;
        }
        // Each lag overlaps fewer frames, so scale it back up.
        let length = window.len() as f32;
        for (lag, value) in self.autocorrelation[..=self.max_lag].iter_mut().enumerate() {
            *value *= length / (length - lag as f32) / energy;
        }

        let hz_to_bin = self.size as f64 / self.fps;
        self.strength.clear();
        self.strength.extend(
            grid.iter()
                .map(|bpm| interpolate(&self.power, bpm / 60.0 * hz_to_bin).sqrt()),
        );
        let strongest = self.strength.iter().copied().fold(f32::EPSILON, f32::max);

        // The comb alone cannot tell a tempo from half of it, as beats at 170
        // also land on every tooth at 85. The spectrum can, since evenly spaced
        // beats have no energy below their own frequency. It only gets a veto,
        // not a vote: hats on the off-beat put more energy at double the tempo
        // than at the tempo itself.
        let acf = &self.autocorrelation[..=self.max_lag];
        for i in 0..grid.len() {
            confidence[i] = comb(acf, 60.0 * self.fps / grid[i]);
            let support = (self.strength[i] / strongest / SPECTRAL_FLOOR).min(1.0);
            emission[i] = confidence[i].max(0.0) * support * priors[i];
        }
    }
}

/// Linearly interpolated read at a fractional position, clamped to the end.
fn interpolate(values: &[f32], position: f64) -> f32 {
    let position = position.min((values.len() - 1) as f64);
    let index = position as usize;
    let next = (index + 1).min(values.len() - 1);
    let fraction = (position - index as f64) as f32;
    values[index] * (1.0 - fraction) + values[next] * fraction
}

/// Mean of the autocorrelation at every multiple of `period`. A real tempo has
/// a peak on every tooth, and the far teeth pin the period down far more
/// precisely than the frame spacing alone would.
fn comb(acf: &[f32], period: f64) -> f32 {
    let last = (acf.len() - 1) as f64;
    if period > last {
        return 0.0;
    }
    let teeth = ((last / period) as usize).min(MAX_COMB_TEETH);
    let sum: f32 = (1..=teeth)
        .map(|k| interpolate(acf, k as f64 * period))
        .sum();
    sum / teeth as f32
}

/// Viterbi search for the highest-scoring tempo path. `scores` holds `bins`
/// values per window.
fn best_path(scores: &[f32], bins: usize) -> Vec<usize> {
    let reach = (JUMP_COST / SLOPE_COST) as usize;
    let windows = scores.len() / bins;
    let mut totals = scores[..bins].to_vec();
    let mut next = vec![0.0; bins];
    let mut from = vec![0u32; windows * bins];

    for t in 1..windows {
        let (best_bin, best_total) = totals
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("the grid is never empty");
        for bin in 0..bins {
            let mut total = best_total - JUMP_COST;
            let mut source = best_bin;
            let near = bin.saturating_sub(reach)..(bin + reach + 1).min(bins);
            for (near, &previous) in totals.iter().enumerate().take(near.end).skip(near.start) {
                let candidate = previous - SLOPE_COST * near.abs_diff(bin) as f32;
                if candidate > total {
                    total = candidate;
                    source = near;
                }
            }
            next[bin] = total + scores[t * bins + bin];
            from[t * bins + bin] = source as u32;
        }
        std::mem::swap(&mut totals, &mut next);
    }

    let mut bin = totals
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(bin, _)| bin)
        .expect("the grid is never empty");
    let mut path = vec![0; windows];
    for t in (0..windows).rev() {
        path[t] = bin;
        bin = from[t * bins + bin] as usize;
    }
    path
}

/// Fit a parabola through a grid point and its neighbours to get between bins.
fn refine(grid: &[f64], scores: &[f32], bin: usize) -> f64 {
    if bin == 0 || bin + 1 == grid.len() {
        return grid[bin];
    }
    let (left, middle, right) = (scores[bin - 1], scores[bin], scores[bin + 1]);
    let curvature = left - 2.0 * middle + right;
    if curvature >= 0.0 {
        return grid[bin];
    }
    let offset = (0.5 * (left - right) / curvature).clamp(-1.0, 1.0);
    grid[bin] * GRID_STEP.powf(f64::from(offset))
}

/// Music almost never truly changes speed by exactly double, but a reading can
/// slip to another level of the pulse for a few bars. Left alone that makes the
/// peak twice the real tempo and the average a blend of two, so readings at a
/// metrical ratio of the track's main tempo are moved back onto it.
fn fold_to_dominant(curve: &mut [f64], beats: &[usize]) {
    let tempos: Vec<f64> = beats.iter().map(|&t| curve[t]).collect();
    let count = tempos.len();
    let Some((dominant, support)) = dominant_tempo(tempos) else {
        return;
    };
    // Without a clear main tempo, as in a long accelerando, there is nothing
    // to call a slip, and folding would cut off genuine changes.
    if (support as f64) < MIN_DOMINANT_SHARE * count as f64 {
        return;
    }
    for &t in beats {
        let nearest = METRICAL_RATIOS
            .iter()
            .find(|&&ratio| (curve[t] / (dominant * ratio) - 1.0).abs() <= SAME_TEMPO);
        if let Some(ratio) = nearest {
            curve[t] /= ratio;
        }
    }
}

/// The tempo the most windows agree on, and how many of them do.
fn dominant_tempo(mut tempos: Vec<f64>) -> Option<(f64, usize)> {
    tempos.sort_by(f64::total_cmp);
    let mut best = 0..0;
    let mut start = 0;
    for end in 0..tempos.len() {
        while tempos[end] > tempos[start] * (1.0 + 2.0 * SAME_TEMPO) {
            start += 1;
        }
        if end + 1 - start > best.len() {
            best = start..end + 1;
        }
    }
    let middle = tempos.get(best.start + best.len() / 2)?;
    Some((*middle, best.len()))
}

/// Indices of the `true` entries that sit in a run of at least `min_run`.
fn sustained(flags: &[bool], min_run: usize) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut start = 0;
    for run in flags.chunk_by(PartialEq::eq) {
        if run[0] && run.len() >= min_run {
            kept.extend(start..start + run.len());
        }
        start += run.len();
    }
    kept
}

fn median_filter(values: &[f64], radius: usize) -> Vec<f64> {
    (0..values.len())
        .map(|i| {
            let start = i.saturating_sub(radius);
            let end = (i + radius + 1).min(values.len());
            let mut around = values[start..end].to_vec();
            around.sort_by(f64::total_cmp);
            around[around.len() / 2]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sustained_drops_short_runs_only() {
        let flags = [true, false, true, true, true, false, true, true];
        assert_eq!(sustained(&flags, 3), [2, 3, 4]);
        assert_eq!(sustained(&flags, 2), [2, 3, 4, 6, 7]);
    }

    #[test]
    fn comb_abstains_on_periods_the_window_cannot_see() {
        assert_eq!(comb(&[1.0; 100], 150.0), 0.0);
        assert_eq!(comb(&[1.0; 100], 50.0), 1.0);
    }

    #[test]
    fn fold_returns_slips_to_the_main_tempo() {
        let mut curve = vec![112.0; 20];
        curve[5..9].fill(224.0);
        curve[12..14].fill(168.0);
        curve[16..18].fill(112.0 * 4.0 / 3.0);
        let beats: Vec<usize> = (0..20).collect();
        fold_to_dominant(&mut curve, &beats);
        for tempo in curve {
            assert!((tempo - 112.0).abs() < 1e-9, "{tempo}");
        }
    }

    #[test]
    fn fold_keeps_a_real_change_to_an_unrelated_tempo() {
        let mut curve = vec![120.0; 20];
        curve[10..].fill(170.0);
        let expected = curve.clone();
        let beats: Vec<usize> = (0..20).collect();
        fold_to_dominant(&mut curve, &beats);
        assert_eq!(curve, expected);
    }

    #[test]
    fn fold_leaves_a_curve_with_no_main_tempo_alone() {
        let mut curve: Vec<f64> = (0..60).map(|t| 90.0 + f64::from(t)).collect();
        let expected = curve.clone();
        let beats: Vec<usize> = (0..60).collect();
        fold_to_dominant(&mut curve, &beats);
        assert_eq!(curve, expected);
    }

    #[test]
    fn median_filter_removes_a_lone_spike() {
        let filtered = median_filter(&[120.0, 120.0, 240.0, 120.0, 120.0], 2);
        assert_eq!(filtered, [120.0; 5]);
    }

    #[test]
    fn best_path_rides_out_one_bad_window_but_follows_a_real_change() {
        // Two distant tempos: window 2 is a lone misread, window 6 is real.
        let (bins, slow, fast) = (200, 0, 199);
        let winners = [slow, slow, fast, slow, slow, slow, fast, fast, fast, fast];
        let mut scores = vec![0.0; winners.len() * bins];
        for (t, &winner) in winners.iter().enumerate() {
            scores[t * bins + winner] = 0.9;
        }
        let expected = [slow, slow, slow, slow, slow, slow, fast, fast, fast, fast];
        assert_eq!(best_path(&scores, bins), expected);
    }

    #[test]
    fn refine_finds_the_top_of_a_parabola() {
        let grid = [100.0, 100.0 * GRID_STEP, 100.0 * GRID_STEP * GRID_STEP];
        assert_eq!(refine(&grid, &[0.5, 1.0, 0.5], 1), grid[1]);
        assert!(refine(&grid, &[0.5, 1.0, 0.9], 1) > grid[1]);
        assert!(refine(&grid, &[0.9, 1.0, 0.5], 1) < grid[1]);
    }
}
