//! Runs the real binary on real container formats, built with ffmpeg.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Pattern, render, wav_bytes};

const BIN: &str = env!("CARGO_BIN_EXE_media-bpm");
const BPM: f64 = 128.0;

struct Fixture {
    dir: tempfile::TempDir,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.wav");
        std::fs::write(&source, wav_bytes(&render(Pattern::Rock, 20.0, |_| BPM))).unwrap();
        Self { dir, source }
    }

    /// Encode the source with extra ffmpeg `args`, into a file called `name`.
    fn encode(&self, name: &str, args: &[&str]) -> PathBuf {
        let path = self.dir.path().join(name);
        let output = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y"])
            .args(args.iter().take_while(|a| **a != "--"))
            .arg("-i")
            .arg(&self.source)
            .args(args.iter().skip_while(|a| **a != "--").skip(1))
            .arg(&path)
            .output()
            .expect("the tests need ffmpeg");
        assert!(
            output.status.success(),
            "ffmpeg could not build {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        path
    }
}

/// A black video stream to mux in ahead of the audio, as ffmpeg input args.
const VIDEO: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "color=black:size=64x64:rate=10:duration=20",
];

fn with_video(tail: &[&'static str]) -> Vec<&'static str> {
    let mut args = VIDEO.to_vec();
    args.push("--");
    args.extend_from_slice(tail);
    args
}

fn run(args: &[&Path], path_var: Option<&str>) -> Output {
    let mut command = Command::new(BIN);
    command.args(args);
    if let Some(path_var) = path_var {
        command.env("PATH", path_var);
    }
    command.output().unwrap()
}

fn average_of(output: &Output) -> f64 {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("average:"))
        .unwrap_or_else(|| {
            panic!(
                "no average in: {stdout} / {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn assert_detects_tempo(path: &Path) {
    let output = run(&[path], None);
    assert!(
        output.status.success(),
        "{}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let average = average_of(&output);
    assert!(
        (average - BPM).abs() < 1.0,
        "{}: got {average}",
        path.display()
    );
}

#[test]
fn audio_containers() {
    let fixture = Fixture::new();
    assert_detects_tempo(&fixture.source);
    assert_detects_tempo(&fixture.encode("a.mp3", &["--", "-c:a", "libmp3lame", "-b:a", "128k"]));
    assert_detects_tempo(&fixture.encode("a.m4a", &["--", "-c:a", "aac"]));
    assert_detects_tempo(&fixture.encode("a.flac", &["--", "-c:a", "flac"]));
    assert_detects_tempo(&fixture.encode("a.ogg", &["--", "-c:a", "libvorbis"]));
    assert_detects_tempo(&fixture.encode("a.opus", &["--", "-c:a", "libopus"]));
    assert_detects_tempo(&fixture.encode("stereo.mp3", &["--", "-ac", "2", "-ar", "48000"]));
    assert_detects_tempo(&fixture.encode("low-rate.wav", &["--", "-ar", "8000"]));
}

#[test]
fn video_containers() {
    let fixture = Fixture::new();
    let mp4 = with_video(&["-c:v", "mpeg4", "-c:a", "aac", "-shortest"]);
    let mkv_aac = with_video(&["-c:v", "mpeg4", "-c:a", "aac"]);
    let mkv_opus = with_video(&["-c:v", "mpeg4", "-c:a", "libopus"]);
    let mkv_ac3 = with_video(&["-c:v", "mpeg4", "-c:a", "ac3"]);
    let mov = with_video(&["-c:v", "mpeg4", "-c:a", "aac"]);
    assert_detects_tempo(&fixture.encode("v.mp4", &mp4));
    assert_detects_tempo(&fixture.encode("aac.mkv", &mkv_aac));
    assert_detects_tempo(&fixture.encode("opus.mkv", &mkv_opus));
    assert_detects_tempo(&fixture.encode("ac3.mkv", &mkv_ac3));
    assert_detects_tempo(&fixture.encode("v.mov", &mov));
}

#[test]
fn common_formats_need_no_ffmpeg_and_the_rest_say_they_do() {
    let fixture = Fixture::new();
    let mp3 = fixture.encode("a.mp3", &["--", "-c:a", "libmp3lame"]);
    let mkv = fixture.encode("aac.mkv", &with_video(&["-c:v", "mpeg4", "-c:a", "aac"]));
    let opus = fixture.encode("a.opus", &["--", "-c:a", "libopus"]);

    assert!((average_of(&run(&[&mp3], Some(""))) - BPM).abs() < 1.0);
    assert!((average_of(&run(&[&mkv], Some(""))) - BPM).abs() < 1.0);

    let output = run(&[&opus], Some(""));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("needs ffmpeg"), "unhelpful error: {stderr}");
}

#[test]
fn file_names_are_not_read_as_ffmpeg_protocols() {
    let fixture = Fixture::new();
    let tricky = fixture.encode("concat:a.opus", &["--", "-c:a", "libopus", "-f", "ogg"]);
    assert_detects_tempo(&tricky);
}

#[test]
fn a_bad_file_fails_without_stopping_the_rest() {
    let fixture = Fixture::new();
    let junk = fixture.dir.path().join("junk.mp4");
    std::fs::write(&junk, b"this is not a media file").unwrap();
    let missing = fixture.dir.path().join("missing.mp3");

    let output = run(&[&junk, &missing, &fixture.source], None);
    assert!(!output.status.success());
    assert!((average_of(&output) - BPM).abs() < 1.0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("junk.mp4"), "{stderr}");
    assert!(stderr.contains("cannot open"), "{stderr}");
}

#[test]
fn audio_with_no_beat_is_reported_as_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silence.wav");
    std::fs::write(&path, wav_bytes(&vec![0.0; 44_100 * 20])).unwrap();
    let output = run(&[&path], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no steady beat"));
}

#[test]
fn nonsense_options_are_rejected() {
    let output = Command::new(BIN)
        .args(["--min-bpm", "200", "--max-bpm", "100", "x.mp3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--max-bpm"));

    let output = Command::new(BIN)
        .args(["--max-bpm", "inf", "x.mp3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
