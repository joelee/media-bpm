# media-bpm

Estimates the average and peak beats per minute of the audio in a media file.

```console
$ media-bpm Lachaim.mp3
Lachaim.mp3
  average:  148.0 BPM
  peak:     148.2 BPM at 0:27
  duration: 3:31 (beat found in 100%)
```

## Install

```console
$ cargo install --path .
```

Most formats need nothing else: mp3, AAC, FLAC, Vorbis, ALAC and WAV, in `mp4`,
`m4a`, `mov`, `mkv`, `webm`, `ogg` and more. Opus, AC-3, DTS and anything else
the built-in decoders reject are handed to the `ffmpeg` command, so install it
if you work with `mkv` files, where those codecs are common.

## Usage

```console
$ media-bpm [OPTIONS] <FILES>...
```

| Option | Default | Meaning |
|---|---|---|
| `--min-bpm` | 40 | Slowest tempo to consider |
| `--max-bpm` | 240 | Fastest tempo to consider |
| `--window` | 10 | Sliding window in seconds |

The exit code is 1 if any file failed or had no steady beat, and 2 for invalid
options. A failed file does not stop the ones after it.

## What the numbers mean

- **Average** is the mean tempo over the parts of the file that have a beat.
- **Peak** is the fastest tempo *held across one window*, not the shortest gap
  between two beats, so a shorter `--window` reacts faster but is noisier.
- **Beat found in** is how much of the file had a steady beat. Dialogue, silence
  and ambient passages are left out of both numbers. Treat a low figure as a
  warning: the tempo was read from only part of the file.

## How it works

1. Decode the audio and reduce it to an onset envelope as it streams, so memory
   stays flat. A two-hour file takes about 5 seconds and under 50 MB.
2. Score every candidate tempo in overlapping windows, using an autocorrelation
   comb, a spectral check that rejects half-time readings, and a preference for
   tempos near 120 BPM.
3. Find the best path through time with a Viterbi search, so one misread window
   cannot become the peak.
4. Move readings that slipped to a metrical ratio of the track's main tempo
   (2:1, 3:2, 4:3 and so on) back onto it.

## Accuracy

Measured against the composer-published tempos of random samples from Kevin
MacLeod's catalogue on incompetech. Acc1 is within 4% of the published tempo.
Acc2 also accepts a reading at a simple ratio of it, such as double or half.

| Sample of 40 tracks | Acc1 | Acc2 | Used for |
|---|---:|---:|---|
| Tuning set | 82% | 95% | Choosing the settings |
| Held-out set | 68% | 78% | Prompted the 4:3 ratio |
| Untouched set | 50% | 88% | Final check only |

The untouched set is the fairest figure, and a sample of 40 is small. On it,
peak stayed within 4% of average on 36 of the 37 tracks where a beat was found.
10 audiobook chapters of real speech were all correctly reported as having no
steady beat.

Known limits:

- **The tempo can be a metrical level off**, most often a slow track reading
  double (60 as 120). Whether that is wrong can be a matter of opinion, since a
  bossa nova can reasonably be counted at 70 or 140. If you know roughly how
  fast a track is, narrow the range: `--max-bpm 100` for slow tracks,
  `--min-bpm 120` for fast ones. On 7 misread tracks this recovered the true
  tempo for 5 and moved 2 to a different wrong level.
- **Free-timed music is unreliable**, solo rubato piano in particular. Ambient
  pieces often report no beat at all. Coverage is usually low in these cases.
- **A real tempo change at an exact metrical ratio is folded away** when the
  track has one clear main tempo, so a true jump from 100 to 200 BPM reads as a
  steady 100. A track with no main tempo, like a long accelerando, is unaffected.

## Development

```console
$ just          # list the recipes
$ just run "My Song.mp3" --max-bpm 100
$ just check    # formatting, clippy and tests
```

The tests synthesise audio with a known tempo. `tests/cli.rs` needs `ffmpeg`, to
build real container files for the binary to read.
