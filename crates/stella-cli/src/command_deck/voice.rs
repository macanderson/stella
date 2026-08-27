//! The driver side of push-to-talk dictation (ADR 0020).
//!
//! The deck's pure state machine (`stella_tui::voice`) decides *when*; this
//! module does the I/O it may not: opening the microphone on
//! [`WorkspaceInput::VoiceStart`], and on [`WorkspaceInput::VoiceStop`]
//! encoding the capture as WAV and posting it to the configured provider's
//! OpenAI-compatible `audio/transcriptions` endpoint. The answer returns on
//! `deck_tx` — [`Inbound::VoiceTranscript`] or [`Inbound::VoiceFailed`] —
//! bypassing the journal like every other piece of ephemeral chrome: a
//! replayed session must not re-paste an old dictation.
//!
//! Capture is split by platform. On macOS and Windows [`capture`] drives
//! `cpal`, which binds the OS frameworks (CoreAudio, WASAPI) and needs no
//! system packages. On Linux it spawns `arecord` writing raw PCM — a `cpal`
//! build there would make ALSA headers a build dependency of every
//! `install.sh` source build, and raw output means the recorder can be
//! killed without corrupting anything, since the WAV container is written
//! here afterwards. Audio lives only in memory (or a temp file the Linux
//! path removes); nothing about a dictation persists except the text.
//!
//! The transcription request is one multipart POST built by hand
//! ([`multipart_wav`]): the shape is a page of well-specified bytes, and
//! writing it here keeps `reqwest`'s `multipart` feature (and its
//! transitive dependencies) out of the workspace.

use std::collections::BTreeMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::settings::{ProviderSettings, Settings, VoiceSettings};
use stella_tui::{Inbound, WorkspaceInput};

/// The transcription model when `voice.model` is unset: the one slug every
/// OpenAI-compatible transcription server recognises.
const DEFAULT_MODEL: &str = "whisper-1";

/// The provider id when `voice.provider` is unset.
const DEFAULT_PROVIDER: &str = "openai";

/// Ceiling on one capture, matching the deck's `stella_tui::voice::MAX_HOLD_MS`
/// — the recorder stops accumulating even if the stop never arrives.
const MAX_CAPTURE_SECS: u32 = 120;

/// Whether dictation is switched on for this workspace (`voice.enabled`) —
/// threaded into `DeckOptions` so the deck's machine never arms when holding
/// space is meant to type spaces.
pub(super) fn enabled(cfg: &Config) -> bool {
    Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.voice.as_ref().and_then(|v| v.enabled))
        .unwrap_or(false)
}

/// One in-flight capture, or nothing. Held by `run_deck_session` across the
/// whole session; transcription tasks are spawned and own their own halves.
#[derive(Default)]
pub(super) struct VoiceLane {
    active: Option<(capture::Capture, Target)>,
}

/// Everything a transcription request needs, resolved before the microphone
/// opens so a configuration error reports immediately instead of after the
/// user has spoken.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// `<base_url>/audio/transcriptions`.
    url: String,
    model: String,
    language: Option<String>,
    api_key: String,
}

/// Serve the three voice inputs; `false` hands anything else back to the
/// caller's chain, the same contract as `service_undo_delete` and its
/// siblings.
pub(super) fn service(
    input: &WorkspaceInput,
    lane: &mut VoiceLane,
    deck_tx: &UnboundedSender<Inbound>,
    cfg: &Config,
) -> bool {
    match input {
        WorkspaceInput::VoiceStart => {
            let outcome = resolve_target(cfg).and_then(|target| {
                capture::Capture::start(MAX_CAPTURE_SECS).map(|cap| (cap, target))
            });
            match outcome {
                Ok(active) => lane.active = Some(active),
                Err(reason) => {
                    let _ = deck_tx.send(Inbound::VoiceFailed { reason });
                }
            }
            true
        }
        WorkspaceInput::VoiceStop => {
            // A stop with nothing running is the tail of a start that
            // already failed (and already answered); silence is right.
            if let Some((cap, target)) = lane.active.take() {
                let tx = deck_tx.clone();
                tokio::spawn(async move {
                    let audio = tokio::task::spawn_blocking(move || cap.finish())
                        .await
                        .map_err(|e| format!("voice: capture task failed: {e}"))
                        .and_then(|r| r);
                    let answer = match audio {
                        Ok(wav) => match transcribe(&target, wav).await {
                            Ok(text) if text.trim().is_empty() => Inbound::VoiceFailed {
                                reason: "voice: heard nothing to transcribe".to_string(),
                            },
                            Ok(text) => Inbound::VoiceTranscript { text },
                            Err(reason) => Inbound::VoiceFailed { reason },
                        },
                        Err(reason) => Inbound::VoiceFailed { reason },
                    };
                    let _ = tx.send(answer);
                });
            }
            true
        }
        WorkspaceInput::VoiceCancel => {
            if let Some((cap, _)) = lane.active.take() {
                tokio::task::spawn_blocking(move || cap.abort());
            }
            true
        }
        _ => false,
    }
}

/// Resolve where a dictation transcribes, from the merged settings: the
/// `voice` section names a provider id; the provider's own entry (or the
/// built-in table) supplies the endpoint and the credential chain, so there
/// is no second secret store. The pure half is [`target_without_key`].
fn resolve_target(cfg: &Config) -> Result<Target, String> {
    let settings = Settings::load(&cfg.workspace_root)
        .map_err(|e| format!("voice: cannot read settings: {e}"))?;
    let voice = settings.voice.clone().unwrap_or_default();
    if !voice.enabled.unwrap_or(false) {
        // Reachable only if the deck armed while settings changed under it.
        return Err("voice: dictation is disabled (`voice.enabled`)".to_string());
    }
    let (unkeyed, provider_id, env_var, inline_key) =
        target_without_key(&voice, &settings.providers)?;
    let credentials = stella_model::credential::CredentialsFile::load_default().ok();
    let (key, _source) = stella_model::credential::ApiKey::resolve(
        &provider_id,
        &env_var,
        inline_key.as_deref(),
        credentials.as_ref(),
        // Never prompt: a dictation is mid-gesture, not a login flow.
        false,
    )
    .map_err(|e| format!("voice: {e}"))?;
    Ok(Target {
        url: unkeyed.url,
        model: unkeyed.model,
        language: unkeyed.language,
        api_key: key.reveal().to_string(),
    })
}

/// [`Target`] minus the secret — everything derivable from settings alone,
/// split out so it is testable without touching the environment or
/// `~/.stella`. Returns the target, the provider id, the env var the key
/// resolves from, and any inline `api_key` the provider entry carries.
fn target_without_key(
    voice: &VoiceSettings,
    providers: &BTreeMap<String, ProviderSettings>,
) -> Result<(Target, String, String, Option<String>), String> {
    let id = voice
        .provider
        .clone()
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());
    let entry = providers.get(&id);
    let builtin = crate::config::PROVIDERS.iter().find(|p| p.id == id);
    let base_url = entry
        .and_then(|e| e.base_url.clone())
        .or_else(|| builtin.map(|p| p.base_url.to_string()))
        .ok_or_else(|| {
            format!(
                "voice: provider `{id}` has no base_url — declare `providers.{id}.base_url` \
                 in settings, or set `voice.provider` to one that has an \
                 OpenAI-compatible `audio/transcriptions` endpoint"
            )
        })?;
    let env_var = entry
        .and_then(|e| e.api_key_env.clone())
        .or_else(|| builtin.map(|p| p.env_var.to_string()))
        .unwrap_or_else(|| crate::config::derived_env_var(&id));
    let inline_key = entry.and_then(|e| e.api_key.clone());
    let target = Target {
        url: format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
        model: voice
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        language: voice.language.clone(),
        api_key: String::new(),
    };
    Ok((target, id, env_var, inline_key))
}

/// POST the capture and return the transcript. One request per dictation, so
/// the client is built here; sixty seconds is generous for a two-minute clip
/// against any of the compatible servers.
async fn transcribe(target: &Target, wav: Vec<u8>) -> Result<String, String> {
    let mut fields = vec![
        ("model".to_string(), target.model.clone()),
        ("response_format".to_string(), "json".to_string()),
    ];
    if let Some(language) = &target.language {
        fields.push(("language".to_string(), language.clone()));
    }
    let boundary = boundary_for(&wav);
    let body = multipart_wav(&boundary, &fields, &wav);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("voice: http client: {e}"))?;
    let response = client
        .post(&target.url)
        .bearer_auth(&target.api_key)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("voice: transcription request failed: {e}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = text.chars().take(200).collect();
        return Err(format!(
            "voice: transcription refused ({status}): {snippet}"
        ));
    }
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("voice: unparseable transcription response: {e}"))?;
    parsed
        .get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .ok_or_else(|| "voice: transcription response carries no `text`".to_string())
}

/// A boundary the payload provably does not contain: the fixed stem grows
/// until no window of the audio matches it. Deterministic, and the scan is
/// one pass per attempt over a payload capped by [`MAX_CAPTURE_SECS`].
fn boundary_for(payload: &[u8]) -> String {
    let mut boundary = String::from("stella-voice-boundary");
    while payload
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary.push('x');
    }
    boundary
}

/// One `multipart/form-data` body: the text fields, then the capture as a
/// `file` part named `dictation.wav`, closed with the final boundary. CRLF
/// throughout — the one detail servers actually reject over.
fn multipart_wav(boundary: &str, fields: &[(String, String)], wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// PCM16 → a complete WAV file. Written here rather than pulling an audio
/// crate: the RIFF header is 44 fixed bytes, and owning it is what lets the
/// Linux recorder emit raw PCM and be killed safely (module docs).
fn wav_from_pcm16(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * u32::from(channels) * 2;
    let block_align = channels * 2;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// Average interleaved frames down to one channel. Halves (or better) the
/// upload, which is what keeps a two-minute capture at a 48kHz stereo
/// device default inside the compatible servers' upload ceilings.
///
/// The `cfg` mirrors its one production caller, the `cpal` capture path —
/// the Linux recorder is told to record mono at the source — while `test`
/// keeps the unit test compiling on every platform CI runs.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn downmix_to_mono(channels: u16, samples: &[i16]) -> Vec<i16> {
    let n = usize::from(channels.max(1));
    if n == 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(n)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|&s| i32::from(s)).sum();
            (sum / n as i32) as i16
        })
        .collect()
}

/// Platform capture behind one three-verb API: `start`, `finish` (blocking:
/// join and encode), `abort` (blocking: join and discard).
mod capture {
    /// macOS / Windows: `cpal` on the OS's own audio framework. The stream
    /// is not `Send`, so a dedicated thread owns device, stream, and buffer,
    /// and parks on the stop channel; dropping the sender is the stop.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(super) struct Capture {
        stop: std::sync::mpsc::Sender<()>,
        done: std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    impl Capture {
        pub(super) fn start(max_secs: u32) -> Result<Self, String> {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
            let (stop, stop_rx) = std::sync::mpsc::channel::<()>();
            let (done_tx, done) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let outcome = (|| -> Result<Vec<u8>, String> {
                    let device = cpal::default_host()
                        .default_input_device()
                        .ok_or("voice: no input device (microphone) available")?;
                    let config = device
                        .default_input_config()
                        .map_err(|e| format!("voice: no input config: {e}"))?;
                    let sample_rate = config.sample_rate().0;
                    let channels = config.channels();
                    let cap = sample_rate as usize * channels as usize * max_secs as usize;
                    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i16>::new()));
                    let sink = std::sync::Arc::clone(&buffer);
                    // A mid-capture stream error (device unplugged, service
                    // restarted) ends the useful audio; the capture then
                    // finishes short and the transcription step reports
                    // "heard nothing", which is the user-visible truth.
                    let on_err = |_e: cpal::StreamError| {};
                    let stream = match config.sample_format() {
                        cpal::SampleFormat::I16 => device.build_input_stream(
                            &config.into(),
                            move |data: &[i16], _| push_capped(&sink, data.iter().copied(), cap),
                            on_err,
                            None,
                        ),
                        cpal::SampleFormat::F32 => device.build_input_stream(
                            &config.into(),
                            move |data: &[f32], _| {
                                push_capped(
                                    &sink,
                                    data.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
                                    cap,
                                )
                            },
                            on_err,
                            None,
                        ),
                        other => {
                            return Err(format!("voice: unsupported sample format {other:?}"));
                        }
                    }
                    .map_err(|e| format!("voice: cannot open microphone: {e}"))?;
                    stream
                        .play()
                        .map_err(|e| format!("voice: cannot start capture: {e}"))?;
                    // Park until the driver stops or aborts (either way the
                    // sender side hangs up); then the stream drops first.
                    let _ = stop_rx.recv();
                    drop(stream);
                    let samples = buffer.lock().unwrap_or_else(|p| p.into_inner());
                    let mono = super::downmix_to_mono(channels, &samples);
                    Ok(super::wav_from_pcm16(sample_rate, 1, &mono))
                })();
                let _ = done_tx.send(outcome);
            });
            Ok(Self { stop, done })
        }

        pub(super) fn finish(self) -> Result<Vec<u8>, String> {
            drop(self.stop);
            self.done
                .recv()
                .map_err(|_| "voice: capture thread died".to_string())
                .and_then(|r| r)
        }

        pub(super) fn abort(self) {
            drop(self.stop);
            let _ = self.done.recv();
        }
    }

    /// Feed the shared buffer, ignoring everything past the cap so a stuck
    /// stop cannot grow memory without bound.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn push_capped(
        sink: &std::sync::Arc<std::sync::Mutex<Vec<i16>>>,
        data: impl Iterator<Item = i16>,
        cap: usize,
    ) {
        let mut buf = sink.lock().unwrap_or_else(|p| p.into_inner());
        let room = cap.saturating_sub(buf.len());
        buf.extend(data.take(room));
    }

    /// Linux: `arecord` writing RAW PCM (S16LE, 16kHz, mono) to a temp file.
    /// Raw on purpose — `Child::kill` is SIGKILL, which would leave a WAV
    /// container's length fields wrong, and the container is written by
    /// `super::wav_from_pcm16` afterwards regardless.
    #[cfg(target_os = "linux")]
    pub(super) struct Capture {
        child: std::process::Child,
        path: std::path::PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl Capture {
        pub(super) fn start(max_secs: u32) -> Result<Self, String> {
            let path =
                std::env::temp_dir().join(format!("stella-dictation-{}.pcm", std::process::id()));
            let child = std::process::Command::new("arecord")
                .args([
                    "-q",
                    "-f",
                    "S16_LE",
                    "-r",
                    "16000",
                    "-c",
                    "1",
                    "-t",
                    "raw",
                    "-d",
                    &max_secs.to_string(),
                ])
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    format!(
                        "voice: cannot spawn `arecord` ({e}) — install alsa-utils, \
                         or see `voice` in settings"
                    )
                })?;
            Ok(Self { child, path })
        }

        pub(super) fn finish(mut self) -> Result<Vec<u8>, String> {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let raw =
                std::fs::read(&self.path).map_err(|e| format!("voice: no capture written: {e}"))?;
            let _ = std::fs::remove_file(&self.path);
            let samples: Vec<i16> = raw
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            Ok(super::wav_from_pcm16(16_000, 1, &samples))
        }

        pub(super) fn abort(mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Anywhere else: no recorder is wired; the gesture reports why.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub(super) struct Capture {}

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    impl Capture {
        pub(super) fn start(_max_secs: u32) -> Result<Self, String> {
            Err("voice: no audio capture backend on this platform".to_string())
        }
        pub(super) fn finish(self) -> Result<Vec<u8>, String> {
            unreachable!("start never succeeds on this platform")
        }
        pub(super) fn abort(self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wav_header_is_the_44_bytes_every_decoder_expects() {
        let wav = wav_from_pcm16(16_000, 1, &[0, 1, -1]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(wav.len(), 44 + 6);
    }

    #[test]
    fn downmix_averages_interleaved_frames_and_passes_mono_through() {
        assert_eq!(downmix_to_mono(2, &[10, 20, -10, -20]), vec![15, -15]);
        assert_eq!(downmix_to_mono(1, &[3, 4]), vec![3, 4]);
    }

    #[test]
    fn the_boundary_never_occurs_in_the_payload() {
        let clean = b"plain audio bytes";
        assert_eq!(boundary_for(clean), "stella-voice-boundary");
        let hostile = b"...stella-voice-boundary...".to_vec();
        let grown = boundary_for(&hostile);
        assert!(grown.len() > "stella-voice-boundary".len());
        assert!(
            !hostile.windows(grown.len()).any(|w| w == grown.as_bytes()),
            "{grown}"
        );
    }

    #[test]
    fn the_multipart_body_carries_fields_then_the_file_then_the_close() {
        let fields = vec![
            ("model".to_string(), "whisper-1".to_string()),
            ("language".to_string(), "en".to_string()),
        ];
        let body = multipart_wav("B", &fields, b"RIFFxxxx");
        let text = String::from_utf8_lossy(&body);
        let model_at = text.find("name=\"model\"\r\n\r\nwhisper-1").unwrap();
        let lang_at = text.find("name=\"language\"\r\n\r\nen").unwrap();
        let file_at = text
            .find(
                "name=\"file\"; filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF",
            )
            .unwrap();
        assert!(model_at < lang_at && lang_at < file_at);
        assert!(text.ends_with("\r\n--B--\r\n"));
    }

    #[test]
    fn the_target_derives_from_the_builtin_table_when_settings_are_silent() {
        let voice = VoiceSettings {
            enabled: Some(true),
            ..VoiceSettings::default()
        };
        let (target, id, env_var, inline) = target_without_key(&voice, &BTreeMap::new()).unwrap();
        assert_eq!(id, "openai");
        assert_eq!(env_var, "OPENAI_API_KEY");
        assert_eq!(target.url, "https://api.openai.com/v1/audio/transcriptions");
        assert_eq!(target.model, "whisper-1");
        assert_eq!(target.language, None);
        assert_eq!(inline, None);
    }

    #[test]
    fn a_declared_provider_entry_overrides_endpoint_and_env() {
        let voice = VoiceSettings {
            enabled: Some(true),
            provider: Some("groq".to_string()),
            model: Some("whisper-large-v3".to_string()),
            language: Some("en".to_string()),
        };
        let mut providers = BTreeMap::new();
        providers.insert(
            "groq".to_string(),
            ProviderSettings {
                base_url: Some("https://api.groq.com/openai/v1/".to_string()),
                api_key_env: Some("GROQ_API_KEY".to_string()),
                ..ProviderSettings::default()
            },
        );
        let (target, id, env_var, _) = target_without_key(&voice, &providers).unwrap();
        assert_eq!(id, "groq");
        assert_eq!(env_var, "GROQ_API_KEY");
        assert_eq!(
            target.url, "https://api.groq.com/openai/v1/audio/transcriptions",
            "a trailing slash must not double up"
        );
        assert_eq!(target.model, "whisper-large-v3");
        assert_eq!(target.language.as_deref(), Some("en"));
    }

    #[test]
    fn an_unknown_provider_with_no_base_url_reports_what_to_declare() {
        let voice = VoiceSettings {
            enabled: Some(true),
            provider: Some("mystt".to_string()),
            ..VoiceSettings::default()
        };
        let err = target_without_key(&voice, &BTreeMap::new()).unwrap_err();
        assert!(err.contains("providers.mystt.base_url"), "{err}");
    }

    /// **The wire witness.** The request `transcribe` sends is what an
    /// OpenAI-compatible server accepts — bearer auth, multipart with the
    /// model field and the WAV bytes — and the `text` of its answer is what
    /// comes back.
    #[tokio::test]
    async fn transcribe_posts_multipart_wav_and_returns_the_text() {
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header_exists("authorization"))
            .and(|req: &Request| {
                let body = &req.body;
                let has = |needle: &[u8]| body.windows(needle.len()).any(|w| w == needle);
                has(b"name=\"model\"\r\n\r\nwhisper-1") && has(b"RIFF")
            })
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "text": " hello stella " })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let target = Target {
            url: format!("{}/v1/audio/transcriptions", server.uri()),
            model: "whisper-1".to_string(),
            language: None,
            api_key: "sk-test".to_string(),
        };
        let wav = wav_from_pcm16(16_000, 1, &[0i16; 8]);
        let text = transcribe(&target, wav).await.unwrap();
        assert_eq!(text, "hello stella");
    }

    #[tokio::test]
    async fn a_refused_transcription_reports_status_and_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;
        let target = Target {
            url: format!("{}/v1/audio/transcriptions", server.uri()),
            model: "whisper-1".to_string(),
            language: None,
            api_key: "sk-wrong".to_string(),
        };
        let err = transcribe(&target, Vec::new()).await.unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("bad key"), "{err}");
    }
}
