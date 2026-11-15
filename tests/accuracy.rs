//! End-to-end tempo checks on synthesised audio with a known tempo.

mod common;

use common::{Noise, Pattern, RATE, envelope, render, speech};
use media_bpm::{Options, Report, tempo};

fn analyze(samples: &[f32]) -> Option<Report> {
    tempo::estimate(&envelope(samples), &Options::default())
}

fn mix(a: &[f32], b: &[f32], b_gain: f32) -> Vec<f32> {
    a.iter().zip(b).map(|(a, b)| a + b * b_gain).collect()
}

fn gain(samples: &[f32], gain: f32) -> Vec<f32> {
    samples.iter().map(|s| s * gain).collect()
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: got {actual:.2}, wanted {expected:.2}"
    );
}

#[test]
fn constant_tempos_are_accurate_and_not_octave_shifted() {
    let patterns = [
        ("clicks", Pattern::Clicks),
        ("four on the floor", Pattern::FourOnFloor),
        ("rock", Pattern::Rock),
        ("shuffle", Pattern::Shuffle),
    ];
    let mut wrong = Vec::new();
    for (name, pattern) in patterns {
        for bpm in [80.0, 100.0, 120.0, 128.0, 140.0, 174.0, 200.0] {
            let report = analyze(&render(pattern, 30.0, |_| bpm)).expect("beat");
            // Peak must also not drift up from the average on a steady track.
            let steady = (report.peak_bpm - report.average_bpm).abs() <= 0.5;
            if (report.average_bpm - bpm).abs() > 0.5 || !steady || report.coverage != 1.0 {
                wrong.push(format!("{name} at {bpm}: {report:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A sixteenth-note line over the kick is a pulse in its own right, so which
/// octave is "the" tempo is a judgement call. The prior and spectral floor are
/// tuned on real recordings, which settle some of these an octave away, so this
/// only insists on the right pulse. `--max-bpm` picks the octave when it matters.
#[test]
fn busy_arrangements_find_the_pulse_up_to_an_octave() {
    let mut wrong = Vec::new();
    for bpm in [80.0, 100.0, 120.0, 128.0, 140.0, 174.0, 200.0] {
        let report = analyze(&render(Pattern::Dance, 30.0, |_| bpm)).expect("beat");
        let octaves = (report.average_bpm / bpm).log2();
        if (octaves - octaves.round()).abs() > 0.01 {
            wrong.push(format!("dance at {bpm}: {report:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn peak_finds_a_short_fast_section_and_says_when() {
    let track = render(Pattern::Rock, 120.0, |t| {
        if (50.0..58.0).contains(&t) {
            170.0
        } else {
            120.0
        }
    });
    let report = analyze(&track).expect("beat");
    assert_close(report.peak_bpm, 170.0, 1.0, "peak");
    assert_close(report.peak_at_secs, 54.0, 5.0, "peak time");
    assert!(
        report.average_bpm < 130.0,
        "average {:.1} should stay near 120",
        report.average_bpm
    );
}

#[test]
fn average_weights_sections_by_length() {
    let track = render(
        Pattern::Rock,
        120.0,
        |t| if t < 60.0 { 100.0 } else { 140.0 },
    );
    let report = analyze(&track).expect("beat");
    assert_close(report.average_bpm, 120.0, 1.0, "average");
    assert_close(report.peak_bpm, 140.0, 0.5, "peak");
}

#[test]
fn gradual_tempo_change_is_tracked() {
    let report = analyze(&render(Pattern::Rock, 120.0, |t| 90.0 + t / 2.0)).expect("beat");
    assert_close(report.average_bpm, 120.0, 1.5, "average");
    // The last window is centred 5 s from the end, where the tempo is 147.5.
    assert_close(report.peak_bpm, 147.5, 2.0, "peak");
}

#[test]
fn audio_without_a_beat_reports_nothing() {
    let rate = RATE as usize;
    let mut noise = Noise::new(7);
    let sine = |hz: f32, level: f32| -> Vec<f32> {
        (0..rate * 60)
            .map(|i| level * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    };
    assert!(analyze(&vec![0.0; rate * 60]).is_none(), "silence");
    assert!(
        analyze(
            &(0..rate * 60)
                .map(|_| 0.3 * noise.next())
                .collect::<Vec<_>>()
        )
        .is_none(),
        "noise"
    );
    assert!(analyze(&sine(440.0, 0.5)).is_none(), "steady tone");
    assert!(analyze(&sine(50.0, 0.02)).is_none(), "mains hum");
    for seed in 1..=4 {
        assert!(analyze(&speech(60, seed, 0.3)).is_none(), "speech {seed}");
    }
}

#[test]
fn music_survives_speech_and_noise_on_top() {
    let mut noise = Noise::new(5);
    let hiss: Vec<f32> = (0..60 * RATE as usize).map(|_| noise.next()).collect();
    let rock = render(Pattern::Rock, 60.0, |_| 128.0);
    let shuffle = render(Pattern::Shuffle, 60.0, |_| 104.0);

    let report = analyze(&mix(&rock, &speech(60, 2, 0.6), 1.0)).expect("beat");
    assert_close(report.average_bpm, 128.0, 0.5, "under speech");
    let report = analyze(&mix(&gain(&rock, 0.25), &speech(60, 3, 0.6), 1.0)).expect("beat");
    assert_close(report.average_bpm, 128.0, 0.5, "quiet under loud speech");
    let report = analyze(&mix(&shuffle, &hiss, 0.4)).expect("beat");
    assert_close(report.average_bpm, 104.0, 0.5, "under noise");
}

#[test]
fn volume_does_not_change_the_answer() {
    let rock = render(Pattern::Rock, 30.0, |_| 128.0);
    for level in [0.1, 0.01, 0.001] {
        let report = analyze(&gain(&rock, level)).expect("beat");
        assert_close(report.average_bpm, 128.0, 0.5, &format!("at gain {level}"));
    }
}

#[test]
fn stretches_without_a_beat_are_left_out_and_reported() {
    let mut track = render(Pattern::Rock, 60.0, |_| 128.0);
    track.resize(120 * RATE as usize, 0.0);
    let report = analyze(&track).expect("beat");
    assert_close(report.average_bpm, 128.0, 0.5, "average");
    assert_close(report.coverage, 0.5, 0.1, "coverage");
    assert_close(report.duration_secs, 120.0, 0.1, "duration");
}

#[test]
fn tempo_range_settles_an_ambiguous_track() {
    let track = render(Pattern::Dance, 30.0, |_| 70.0);
    let options = Options {
        max_bpm: 100.0,
        ..Options::default()
    };
    let report = tempo::estimate(&envelope(&track), &options).expect("beat");
    assert_close(report.average_bpm, 70.0, 0.5, "average");
}

#[test]
fn clips_shorter_than_the_window_still_work() {
    let report = analyze(&render(Pattern::Rock, 6.0, |_| 128.0)).expect("beat");
    assert_close(report.average_bpm, 128.0, 1.0, "average");
    assert!(
        analyze(&render(Pattern::Rock, 3.0, |_| 128.0)).is_none(),
        "too short to judge"
    );
}
