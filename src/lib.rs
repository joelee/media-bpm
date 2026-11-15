//! Estimate the tempo of the audio in a media file.

pub mod decode;
pub mod onset;
pub mod tempo;

use std::path::Path;

use anyhow::Result;

pub use tempo::{Options, Report};

/// Decode the audio in `path` and estimate its tempo.
///
/// Returns `Ok(None)` when the audio decodes fine but has no steady beat.
pub fn analyze_file(path: &Path, options: &Options) -> Result<Option<Report>> {
    let envelope = decode::onset_envelope(path)?;
    Ok(tempo::estimate(&envelope, options))
}
