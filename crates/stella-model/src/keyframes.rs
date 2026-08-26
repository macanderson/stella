//! Sampling a video into still frames, for a dialect that can see images but
//! has no wire shape for video.
//!
//! [`plan_timestamps`] is pure. Given a duration and a ceiling it returns the
//! exact instants to sample, deterministically, with no I/O.
//! [`FrameSampler`] is the I/O seam: [`FfmpegSampler`] implements it by
//! shelling out to `ffprobe`/`ffmpeg`, and a test supplies its own. The split
//! is what makes the decision checkable without a decoder on the machine
//! running the test.
//!
//! ## Why the degrade is frames rather than a note
//!
//! `crate::attachment`'s contract is that an attachment a dialect cannot
//! ingest degrades to a [`crate::attachment::WirePart::Text`] note rather than
//! erroring. For video on an image-capable dialect that note threw away
//! something the model could genuinely have used: an image-capable model shown
//! eight stills of a clip can answer most questions about its visual content.
//! So the degrade ladder for video is now: native video where the dialect
//! carries it, sampled frames where it carries images, the note otherwise —
//! and the note is still the floor, reached whenever sampling cannot run.
//!
//! ## What the model is told it saw
//!
//! The model must never believe it watched the video. Every sampled degrade
//! rides with a note ([`sampling_note`]) naming the frame count, the
//! timestamps, the duration, and the fact that audio was not transcribed, so
//! the model's answer to the user describes what it actually saw.
//!
//! ## Cost
//!
//! Frames are images, and the conversation replays every turn, so an
//! unbounded or uncached sampler would re-encode a video into the prompt on
//! every model call for the rest of the session. Both are bounded here:
//! [`MAX_SAMPLED_FRAMES`] caps the fan-out and [`plan_timestamps`] asks for
//! fewer on a short clip, while `crate::attachment`'s path cache memoizes the
//! extraction per file version so `ffmpeg` runs once, not once per turn.

use std::path::Path;
use std::process::Command;

/// The most frames one video contributes to a request.
///
/// A ceiling rather than a target: [`plan_timestamps`] asks for roughly one
/// frame per second and stops here, so a five-second clip costs five images
/// and a five-minute one costs eight. Eight is the bound because these are
/// full image attachments on every replayed turn — the number is chosen to
/// keep a video's cost the same order as a handful of screenshots, which is
/// the case the attachment plane was built for.
pub(crate) const MAX_SAMPLED_FRAMES: usize = 8;

/// The longest edge a sampled frame is scaled to, in pixels.
///
/// Frames are evidence of what is on screen, not stills to be zoomed into,
/// and every provider re-scales large images anyway before charging for
/// them. Bounding here keeps eight frames of a 4K clip from dwarfing the rest
/// of the prompt.
pub(crate) const FRAME_LONG_EDGE_PX: u32 = 768;

/// One frame lifted out of a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SampledFrame {
    /// Where in the video this frame was taken, in milliseconds.
    pub at_ms: u64,
    /// The frame's media type — what the adapter puts on the wire.
    pub media_type: String,
    /// The frame's bytes, already base64-encoded.
    pub base64: String,
}

/// A video reduced to stills, with the duration the plan was cut against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SampledVideo {
    pub duration_ms: u64,
    pub frames: Vec<SampledFrame>,
}

impl SampledVideo {
    /// Retained heap bytes, for the attachment path cache's budget.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.base64.len() + frame.media_type.len())
            .sum()
    }
}

/// Why a video could not be sampled.
///
/// The caller branches on which one it got: each becomes a different clause
/// in the degrade note, and the note is what tells the user why the model
/// cannot see their clip. A message string could not be branched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleFailure {
    /// `ffprobe`/`ffmpeg` is not on PATH. Sampling is a probe-and-degrade
    /// capability, never a build or install requirement.
    ToolMissing,
    /// The probe ran but the file's duration could not be read — a container
    /// `ffprobe` does not understand, or a truncated file.
    UnreadableDuration,
    /// Extraction ran and produced no usable frame.
    NoFrames,
    /// The attachment's bytes could not be staged as a file for the decoder
    /// to read — a corrupt inline payload, or a temp directory that cannot be
    /// written. Distinct from the three above because nothing was decoded:
    /// the failure is upstream of `ffmpeg` ever starting (#4800).
    Unstageable,
}

impl SampleFailure {
    /// The clause this failure contributes to the degrade note, phrased for a
    /// model that has to relay it to the user.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            SampleFailure::ToolMissing => {
                "no video decoder (ffmpeg) is installed on the machine running Stella"
            }
            SampleFailure::UnreadableDuration => {
                "the video's duration could not be read, so no frames could be chosen"
            }
            SampleFailure::NoFrames => "no frame could be decoded from the video",
            SampleFailure::Unstageable => {
                "the attached bytes could not be written to a temporary file for the decoder \
                 to read"
            }
        }
    }
}

/// Lifts still frames out of a video file. The I/O half of this module, kept
/// behind a trait so [`crate::attachment`]'s fan-out can be witnessed without
/// a decoder installed.
pub(crate) trait FrameSampler {
    /// Sample at most `max_frames` frames from the video at `path`.
    fn sample(&self, path: &Path, max_frames: usize) -> Result<SampledVideo, SampleFailure>;
}

/// Which instants to sample from a video of `duration_ms`, at most
/// `max_frames` of them.
///
/// Two rules, both deterministic so the plan is a property rather than a
/// guess:
///
/// - **Roughly one frame per second, capped.** A three-second clip does not
///   need eight frames and should not be billed for them; a long one is
///   capped at `max_frames` regardless.
/// - **Segment midpoints.** With `n` frames the video is cut into `n` equal
///   segments and sampled at the middle of each: `t_i = d·(2i+1)/2n`. The
///   ends are what the midpoint avoids — `t = 0` is very often a black or
///   title frame, and `t = d` can land past the last decodable frame, so
///   sampling the extremes spends two of eight frames on nothing.
///
/// A zero-length or unreadable-length video yields a single frame at zero:
/// one still is a better degrade than none, and the caller drops the plan if
/// that frame does not decode.
pub(crate) fn plan_timestamps(duration_ms: u64, max_frames: usize) -> Vec<u64> {
    if max_frames == 0 {
        return Vec::new();
    }
    if duration_ms == 0 {
        return vec![0];
    }
    let per_second = duration_ms.div_ceil(1_000).max(1);
    let n = per_second.min(max_frames as u64);
    (0..n)
        .map(|i| duration_ms.saturating_mul(2 * i + 1) / (2 * n))
        .collect()
}

/// The note that rides with a sampled degrade.
///
/// Its whole job is that the model's answer reflects what the model actually
/// saw: stills, at named instants, with no audio. A model told only "here are
/// some images" would describe the video as if it had watched it.
pub(crate) fn sampling_note(label: &str, video: &SampledVideo) -> String {
    let stamps = video
        .frames
        .iter()
        .map(|frame| format_stamp(frame.at_ms))
        .collect::<Vec<_>>()
        .join(", ");
    let count = video.frames.len();
    let plural = if count == 1 { "frame" } else { "frames" };
    format!(
        "[the user attached the video {label}; the current provider cannot ingest video \
         natively, so {count} still {plural} sampled from it are attached above, taken at \
         {stamps} of a {total} clip. You are seeing sampled stills, NOT the video: motion \
         between frames is not visible and the audio was not transcribed. Answer from the \
         frames and say plainly that is what you saw.]",
        total = format_stamp(video.duration_ms),
    )
}

/// The note for a video that could not be sampled at all — today's total
/// degrade, plus the reason, so the user learns what to install.
pub(crate) fn unsampled_note(label: &str, failure: SampleFailure) -> String {
    format!(
        "[the user attached the video {label}. The current provider cannot ingest video \
         natively, and sampling still frames from it was not possible because {}. \
         Acknowledge the attachment, say why it could not be read, and suggest a provider \
         or format that can be.]",
        failure.reason()
    )
}

/// `m:ss` for a millisecond offset — the shape a person reads off a player.
fn format_stamp(ms: u64) -> String {
    let total_secs = ms / 1_000;
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// The shipping [`FrameSampler`]: `ffprobe` for the duration, one `ffmpeg`
/// seek-and-decode per planned instant.
///
/// `ffmpeg` is probed at call time and never required: a machine without it
/// degrades to [`SampleFailure::ToolMissing`], which the caller turns back
/// into the descriptive note video attachments have always produced.
pub(crate) struct FfmpegSampler;

impl FrameSampler for FfmpegSampler {
    fn sample(&self, path: &Path, max_frames: usize) -> Result<SampledVideo, SampleFailure> {
        let duration_ms = probe_duration_ms(path)?;
        let plan = plan_timestamps(duration_ms, max_frames);
        let frames: Vec<SampledFrame> = plan
            .iter()
            .filter_map(|&at_ms| {
                decode_frame(path, at_ms).map(|base64| SampledFrame {
                    at_ms,
                    media_type: "image/jpeg".to_string(),
                    base64,
                })
            })
            .collect();
        if frames.is_empty() {
            return Err(SampleFailure::NoFrames);
        }
        Ok(SampledVideo {
            duration_ms,
            frames,
        })
    }
}

/// The video's duration in milliseconds, via `ffprobe`.
fn probe_duration_ms(path: &Path) -> Result<u64, SampleFailure> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .map_err(|_| SampleFailure::ToolMissing)?;
    if !output.status.success() {
        return Err(SampleFailure::UnreadableDuration);
    }
    let seconds: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| SampleFailure::UnreadableDuration)?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(SampleFailure::UnreadableDuration);
    }
    // Saturating rather than `as`: a nonsense duration must not wrap into a
    // plausible one and silently produce a plan against a fiction.
    Ok((seconds * 1_000.0).min(u64::MAX as f64) as u64)
}

/// One frame at `at_ms`, JPEG-encoded and base64'd, or `None` if this instant
/// did not decode. A single missing frame is survivable — the caller only
/// gives up when every one of them is.
fn decode_frame(path: &Path, at_ms: u64) -> Option<String> {
    use base64::Engine as _;
    // `-ss` before `-i` seeks by keyframe before decoding, which is what
    // makes eight seeks into a long video cheap instead of eight full
    // decodes.
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss"])
        .arg(format!("{}.{:03}", at_ms / 1_000, at_ms % 1_000))
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!(
                "scale='min({px},iw)':-2:force_original_aspect_ratio=decrease",
                px = FRAME_LONG_EDGE_PX
            ),
            "-q:v",
            "4",
            "-f",
            "image2",
            "-vcodec",
            "mjpeg",
            "-",
        ])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_clip_costs_one_frame_per_second_not_the_ceiling() {
        assert_eq!(plan_timestamps(3_000, 8).len(), 3);
        assert_eq!(plan_timestamps(1_200, 8).len(), 2);
    }

    #[test]
    fn a_long_clip_is_capped_at_the_ceiling() {
        assert_eq!(plan_timestamps(600_000, 8).len(), 8);
        assert_eq!(plan_timestamps(u64::MAX, 8).len(), 8);
    }

    /// The plan samples segment midpoints, so it never spends a frame on
    /// `t = 0` (usually black) or on `t = duration` (often past the last
    /// decodable frame).
    #[test]
    fn the_plan_samples_midpoints_and_never_the_ends() {
        let plan = plan_timestamps(8_000, 4);
        assert_eq!(plan, vec![1_000, 3_000, 5_000, 7_000]);
        for &t in &plan {
            assert!(t > 0 && t < 8_000, "midpoint {t} landed on an end");
        }
    }

    #[test]
    fn the_plan_is_deterministic() {
        for d in [0u64, 1, 999, 1_000, 45_678, 3_600_000] {
            assert_eq!(plan_timestamps(d, 8), plan_timestamps(d, 8), "duration {d}");
        }
    }

    #[test]
    fn a_zero_length_video_still_plans_one_frame() {
        assert_eq!(plan_timestamps(0, 8), vec![0]);
    }

    #[test]
    fn a_zero_ceiling_plans_nothing() {
        assert!(plan_timestamps(60_000, 0).is_empty());
    }

    #[test]
    fn the_note_names_the_count_the_stamps_and_that_audio_was_not_heard() {
        let video = SampledVideo {
            duration_ms: 125_000,
            frames: vec![
                SampledFrame {
                    at_ms: 15_000,
                    media_type: "image/jpeg".into(),
                    base64: "a".into(),
                },
                SampledFrame {
                    at_ms: 95_000,
                    media_type: "image/jpeg".into(),
                    base64: "b".into(),
                },
            ],
        };
        let note = sampling_note("clip.mp4", &video);
        assert!(note.contains("2 still frames"), "{note}");
        assert!(note.contains("0:15, 1:35"), "{note}");
        assert!(note.contains("2:05 clip"), "{note}");
        assert!(note.contains("audio was not transcribed"), "{note}");
        assert!(note.contains("NOT the video"), "{note}");
    }

    /// The one test that runs the real thing: `FfmpegSampler` against a
    /// video `ffmpeg` generates for it. The argument vector, the `-ss`
    /// timestamp spelling, the probe's number parsing and the stdout capture
    /// are settled by nothing else in this crate — the fake sampler in
    /// `crate::attachment` proves the fan-out and never touches a decoder.
    ///
    /// Skipped where `ffmpeg` is absent, which includes CI: `ubuntu-latest`
    /// does not ship one. A skip here is a gap, not a pass — it is recorded
    /// as one rather than dressed up, and the test is written so a dev box
    /// with `ffmpeg` (every machine that will ever change this module) runs
    /// it for real.
    #[test]
    fn the_ffmpeg_sampler_lifts_real_frames_out_of_a_real_video() {
        use base64::Engine as _;
        use std::process::Stdio;
        let have = |bin: &str| {
            Command::new(bin)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !have("ffmpeg") || !have("ffprobe") {
            eprintln!("SKIPPED: no ffmpeg/ffprobe on PATH — the sampler ran against nothing");
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        let made = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=10:duration=6",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .expect("spawn ffmpeg");
        assert!(made.success(), "could not synthesize the fixture clip");

        let video = FfmpegSampler
            .sample(&clip, MAX_SAMPLED_FRAMES)
            .expect("a six-second clip samples");

        assert!(
            (5_900..=6_100).contains(&video.duration_ms),
            "probed duration {}ms is not the six seconds asked for",
            video.duration_ms
        );
        assert_eq!(
            video.frames.len(),
            6,
            "one frame per second of a six-second clip, under the ceiling of {MAX_SAMPLED_FRAMES}"
        );
        assert_eq!(
            video.frames.iter().map(|f| f.at_ms).collect::<Vec<_>>(),
            plan_timestamps(video.duration_ms, MAX_SAMPLED_FRAMES),
            "every planned instant decoded"
        );
        for frame in &video.frames {
            assert_eq!(frame.media_type, "image/jpeg");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&frame.base64)
                .expect("the frame is valid base64");
            assert!(
                bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
                "frame at {}ms is not a JPEG",
                frame.at_ms
            );
        }
    }

    #[test]
    fn every_failure_names_a_reason_the_user_can_act_on() {
        for failure in [
            SampleFailure::ToolMissing,
            SampleFailure::UnreadableDuration,
            SampleFailure::NoFrames,
        ] {
            let note = unsampled_note("clip.mp4", failure);
            assert!(note.contains("clip.mp4"), "{note}");
            assert!(note.contains(failure.reason()), "{note}");
        }
        assert!(
            unsampled_note("clip.mp4", SampleFailure::ToolMissing).contains("ffmpeg"),
            "the missing-tool note must name the tool to install"
        );
    }
}
