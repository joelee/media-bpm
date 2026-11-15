//! Decodes a media file's audio straight into an onset envelope.
//!
//! Symphonia handles what it can so the common case needs no external tools.
//! It has no Opus, AC-3 or DTS decoder though, and those are everywhere in
//! `mkv` files, so anything it rejects goes to the `ffmpeg` command instead.

use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail, ensure};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::onset::{Envelope, OnsetDetector};

/// Rate requested from ffmpeg. Everything the detector looks at is below 8 kHz.
const FFMPEG_SAMPLE_RATE: u32 = 22_050;

pub fn onset_envelope(path: &Path) -> Result<Envelope> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    match decode_native(file, path) {
        Ok(envelope) => Ok(envelope),
        Err(native) => decode_ffmpeg(path)
            .map_err(|ffmpeg| anyhow!("cannot decode audio: {native}; {ffmpeg:#}")),
    }
}

fn decode_native(file: File, path: &Path) -> Result<Envelope> {
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .default_track(TrackType::Audio)
        .context("no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .context("no audio codec parameters")?;
    let mut decoder =
        symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?;

    let mut detector = None;
    let mut interleaved = Vec::new();
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            // Chained Ogg streams signal a reset; the first link is enough.
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(audio) => audio,
            // A damaged packet costs a few milliseconds of audio, not the file.
            Err(SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(e.into()),
        };

        let channels = audio.spec().channels().count().max(1);
        let rate = audio.spec().rate();
        ensure!(rate > 0, "stream reports a sample rate of 0");
        audio.copy_to_vec_interleaved(&mut interleaved);
        mono.clear();
        mono.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame: &[f32]| frame.iter().sum::<f32>() / channels as f32),
        );
        detector
            .get_or_insert_with(|| OnsetDetector::new(rate))
            .push(&mono);
    }
    Ok(detector.context("no audio could be decoded")?.finish())
}

fn decode_ffmpeg(path: &Path) -> Result<Envelope> {
    // The `file:` prefix stops ffmpeg reading names like `concat:a|b` or
    // `http://x` as protocols.
    let mut input = OsString::from("file:");
    input.push(path);
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(input)
        .args(["-vn", "-sn", "-dn", "-ac", "1", "-ar"])
        .arg(FFMPEG_SAMPLE_RATE.to_string())
        .args(["-f", "f32le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => anyhow!("this format needs ffmpeg, which is not installed"),
            _ => anyhow!("cannot run ffmpeg: {e}"),
        })?;

    // Drained on its own thread so a chatty ffmpeg cannot fill the pipe and
    // stall while we are blocked reading stdout.
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut detector = OnsetDetector::new(FFMPEG_SAMPLE_RATE);
    let mut bytes = vec![0u8; 1 << 16];
    let mut filled = 0;
    let mut samples = Vec::new();
    loop {
        let read = stdout
            .read(&mut bytes[filled..])
            .context("cannot read from ffmpeg")?;
        if read == 0 {
            break;
        }
        filled += read;
        // A read can stop partway through a sample; carry the tail over.
        let whole = filled - filled % 4;
        samples.clear();
        let (chunks, _) = bytes[..whole].as_chunks::<4>();
        samples.extend(chunks.iter().map(|&b| f32::from_le_bytes(b)));
        detector.push(&samples);
        bytes.copy_within(whole..filled, 0);
        filled -= whole;
    }

    let status = child.wait().context("cannot wait for ffmpeg")?;
    let stderr = stderr.join().unwrap_or_default();
    let envelope = detector.finish();
    if !status.success() || envelope.values.is_empty() {
        bail!("ffmpeg failed: {}", stderr.trim());
    }
    Ok(envelope)
}
