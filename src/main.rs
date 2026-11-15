use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, ensure};
use clap::Parser;
use media_bpm::{Options, Report, analyze_file};

/// Estimate the average and peak beats per minute of audio and video files.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Media files to analyze (mp3, mp4, mkv, flac, wav, ...)
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Slowest tempo to consider
    #[arg(long, default_value_t = Options::default().min_bpm)]
    min_bpm: f64,

    /// Fastest tempo to consider
    #[arg(long, default_value_t = Options::default().max_bpm)]
    max_bpm: f64,

    /// Sliding window in seconds; peak is the fastest tempo held across one
    #[arg(long, default_value_t = Options::default().window_secs)]
    window: f64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let options = match options(&cli) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };

    let mut failed = false;
    for (i, path) in cli.files.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", path.display());
        match analyze_file(path, &options) {
            Ok(Some(report)) => print_report(&report),
            Ok(None) => {
                eprintln!("error: {}: no steady beat found", path.display());
                failed = true;
            }
            Err(e) => {
                eprintln!("error: {}: {e:#}", path.display());
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn options(cli: &Cli) -> Result<Options> {
    ensure!(cli.min_bpm > 0.0, "--min-bpm must be above 0");
    ensure!(
        cli.max_bpm >= cli.min_bpm * 2.0,
        "--max-bpm must be at least twice --min-bpm"
    );
    ensure!(cli.max_bpm <= 1000.0, "--max-bpm must be at most 1000");
    ensure!(cli.window >= 2.0, "--window must be at least 2 seconds");
    Ok(Options {
        min_bpm: cli.min_bpm,
        max_bpm: cli.max_bpm,
        window_secs: cli.window,
    })
}

fn print_report(report: &Report) {
    println!("  average:  {:.1} BPM", report.average_bpm);
    println!(
        "  peak:     {:.1} BPM at {}",
        report.peak_bpm,
        timestamp(report.peak_at_secs)
    );
    println!(
        "  duration: {} (beat found in {:.0}%)",
        timestamp(report.duration_secs),
        report.coverage * 100.0
    );
}

fn timestamp(secs: f64) -> String {
    let secs = secs.round() as u64;
    match secs / 3600 {
        0 => format!("{}:{:02}", secs / 60, secs % 60),
        hours => format!("{hours}:{:02}:{:02}", secs / 60 % 60, secs % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::timestamp;

    #[test]
    fn timestamps_gain_hours_only_when_needed() {
        assert_eq!(timestamp(5.0), "0:05");
        assert_eq!(timestamp(225.4), "3:45");
        assert_eq!(timestamp(7322.0), "2:02:02");
    }
}
