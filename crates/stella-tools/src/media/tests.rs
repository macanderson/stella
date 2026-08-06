//! Tests for the media tools.
//!
//! Split out of `media.rs` so the parent stays under the file-size
//! ceiling: the port added by #1596 needs a line in a struct literal
//! here, and a grandfathered god file is closed to growth.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use stella_media::{
    CostDecision, JobStore, MediaArtifact, MediaCapabilities, MediaError, MediaJob, MediaJobState,
    MediaJobStatus, MediaKind, MediaSpendGate, MediaSpendRequest, VideoRequest,
};

/// Deterministic provider: one fixed PNG-ish artifact, no network.
struct FakeImages;

#[async_trait]
impl MediaProvider for FakeImages {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities::default()
    }
    async fn generate_image(&self, req: ImageRequest) -> Result<MediaArtifact, MediaError> {
        if req.prompt.contains("refuse me") {
            return Err(MediaError::ContentPolicy("test refusal".into()));
        }
        Ok(MediaArtifact {
            kind: MediaKind::Image,
            bytes: vec![0x89, b'P', b'N', b'G'],
            extension: "png".into(),
            label: req.label,
            model: "fake-image-1".into(),
            cost_usd: 0.01,
        })
    }
    async fn generate_video(&self, _req: VideoRequest) -> Result<MediaJob, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
}

struct CountingImages {
    submits: AtomicUsize,
    manifest_blocker: Option<std::path::PathBuf>,
}

#[async_trait]
impl MediaProvider for CountingImages {
    fn id(&self) -> &str {
        "counting"
    }

    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: "counting".into(),
            image: true,
            image_usd_each: Some(0.01),
            ..Default::default()
        }
    }

    async fn generate_image(&self, req: ImageRequest) -> Result<MediaArtifact, MediaError> {
        let attempt = self.submits.fetch_add(1, Ordering::SeqCst);
        if attempt == 0
            && let Some(blocker) = &self.manifest_blocker
        {
            std::fs::create_dir_all(blocker).unwrap();
        }
        Ok(MediaArtifact {
            kind: MediaKind::Image,
            bytes: vec![0x89, b'P', b'N', b'G'],
            extension: "png".into(),
            label: req.label,
            model: "counting-image-1".into(),
            cost_usd: 0.01,
        })
    }

    async fn generate_video(&self, _req: VideoRequest) -> Result<MediaJob, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }

    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
}

struct ApproveSpendGate {
    calls: AtomicUsize,
}

struct FixedOperationIds(&'static str);

/// One expiry for the whole test binary, minted on first use.
///
/// The journal treats the expiry as part of a claim's identity, so a
/// replay under the same operation key must present a byte-identical
/// expiry to be recognized as the same request. Recomputing `unix_now()`
/// per call raced the next second boundary: two `execute` calls that
/// straddled one landed a claim of `now + 3600` and then `now + 3601`,
/// and the second was rejected as a conflicting identity instead of
/// replaying. Pinning it keeps `FixedOperationIds` as fixed as its name
/// promises. A host does the same thing — see the `SameHostOperation`
/// source in `tests/media_replay.rs`, which carries its expiry as a field.
fn fixed_expiry() -> u64 {
    static EXPIRY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *EXPIRY.get_or_init(|| unix_now() + 3600)
}

impl MediaOperationIdSource for FixedOperationIds {
    fn operation_id(&self) -> HostMediaOperation {
        // The journal folds `expires_at` into a claim's identity, so
        // recomputing it per call would hand the same operation a different
        // deadline as soon as the clock ticked between two `execute`s —
        // the retry then reads as a different request wearing the same key
        // and the claim is rejected. Snapshot the deadline once, process
        // wide, so a "fixed" id source is fixed in both of its fields.
        static EXPIRES_AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        HostMediaOperation {
            opaque_id: self.0.to_string(),
            expires_at: *EXPIRES_AT.get_or_init(|| unix_now() + 3600),
        }
    }
}

/// Guards the helper above, not production code: every retry-safety test
/// here calls `execute` twice against one id source and would pass by luck
/// on a machine fast enough to stay inside a single second. Crossing a
/// second boundary on purpose is the only way to make that luck visible.
#[test]
fn a_fixed_operation_id_source_holds_one_expiry_across_a_clock_tick() {
    let ids = FixedOperationIds("host-stable-expiry");
    let first = ids.operation_id();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = ids.operation_id();
    assert_eq!(
        first.expires_at, second.expires_at,
        "a retry must re-present the same expiry or the journal reads it as \
             a different request wearing the same key",
    );
    assert_eq!(first.opaque_id, second.opaque_id);
}

#[async_trait]
impl MediaSpendGate for ApproveSpendGate {
    async fn authorize(&self, _request: &MediaSpendRequest) -> CostDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        CostDecision::Approve
    }
}

fn approving_gate() -> Arc<dyn MediaSpendGate> {
    Arc::new(ApproveSpendGate {
        calls: AtomicUsize::new(0),
    })
}

fn operation_journal() -> Arc<dyn MediaOperationJournal> {
    Arc::new(stella_media::SqliteMediaOperationJournal::open_in_memory(Default::default()).unwrap())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SubmissionEvent {
    Gate(MediaKind, String),
    Provider(MediaKind, String),
}

struct OrderedGate(Arc<std::sync::Mutex<Vec<SubmissionEvent>>>);

#[async_trait]
impl MediaSpendGate for OrderedGate {
    async fn authorize(&self, request: &MediaSpendRequest) -> CostDecision {
        self.0.lock().unwrap().push(SubmissionEvent::Gate(
            request.kind,
            request.operation_id.clone(),
        ));
        CostDecision::Approve
    }
}

struct OrderedProvider {
    events: Arc<std::sync::Mutex<Vec<SubmissionEvent>>>,
    image_price: Option<f64>,
    video_price: Option<f64>,
}

#[async_trait]
impl MediaProvider for OrderedProvider {
    fn id(&self) -> &str {
        "ordered"
    }

    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: "ordered".into(),
            image: true,
            video: true,
            image_usd_each: self.image_price,
            video_usd_per_second: self.video_price,
            ..Default::default()
        }
    }

    async fn generate_image(&self, req: ImageRequest) -> Result<MediaArtifact, MediaError> {
        self.events.lock().unwrap().push(SubmissionEvent::Provider(
            MediaKind::Image,
            req.operation_id.unwrap(),
        ));
        Ok(MediaArtifact {
            kind: MediaKind::Image,
            bytes: b"image".to_vec(),
            extension: "png".into(),
            label: req.label,
            model: "ordered-image".into(),
            cost_usd: self.image_price.unwrap_or_default(),
        })
    }

    async fn generate_video(&self, req: VideoRequest) -> Result<MediaJob, MediaError> {
        self.events.lock().unwrap().push(SubmissionEvent::Provider(
            MediaKind::Video,
            req.operation_id.unwrap(),
        ));
        Ok(MediaJob {
            artifact_id: "med_ordered".into(),
            provider_id: "ordered".into(),
            provider_job_id: "ordered-job".into(),
            kind: MediaKind::Video,
            model: "ordered-video".into(),
            estimated_cost_usd: self.video_price.unwrap_or_default() * 5.0,
            submitted_at: unix_now(),
            label: req.label,
        })
    }

    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
}

#[tokio::test]
async fn priced_and_unpriced_media_share_gate_then_provider_order() {
    for (kind, price, host_id) in [
        (MediaKind::Image, Some(0.01), "host-priced-image"),
        (MediaKind::Image, None, "host-unpriced-image"),
        (MediaKind::Video, Some(0.2), "host-priced-video"),
        (MediaKind::Video, None, "host-unpriced-video"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(OrderedProvider {
            events: events.clone(),
            image_price: if kind == MediaKind::Image {
                price
            } else {
                None
            },
            video_price: if kind == MediaKind::Video {
                price
            } else {
                None
            },
        });
        let gate = Arc::new(OrderedGate(events.clone()));
        let ids = Arc::new(FixedOperationIds(host_id));
        let journal = operation_journal();
        let tool: Arc<dyn Tool> = match kind {
            MediaKind::Image => Arc::new(GenerateImage::with_host_context(
                provider, gate, ids, journal,
            )),
            MediaKind::Video => Arc::new(GenerateVideo::with_host_context(
                provider, gate, ids, journal,
            )),
            MediaKind::Svg => unreachable!(),
        };
        for _ in 0..2 {
            let out = tool
                .execute(&serde_json::json!({"prompt": "same"}), dir.path())
                .await;
            assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        }
        let actual = events.lock().unwrap().clone();
        assert_eq!(actual.len(), 2, "{kind:?} at {price:?}: {actual:?}");
        let operation_id = match &actual[0] {
            SubmissionEvent::Gate(_, operation_id) => operation_id.clone(),
            event => panic!("gate must be first: {event:?}"),
        };
        assert_eq!(
            actual,
            vec![
                SubmissionEvent::Gate(kind, operation_id.clone()),
                SubmissionEvent::Provider(kind, operation_id),
            ]
        );
    }
}

/// A host that recomputes its expiry from a clock on every call, which is
/// what [`FixedOperationIds`] used to do — the drift is one second, the
/// same step a real clock takes across a boundary.
struct DriftingOperationIds {
    opaque_id: &'static str,
    calls: AtomicUsize,
}

impl MediaOperationIdSource for DriftingOperationIds {
    fn operation_id(&self) -> HostMediaOperation {
        HostMediaOperation {
            opaque_id: self.opaque_id.to_string(),
            expires_at: fixed_expiry() + self.calls.fetch_add(1, Ordering::SeqCst) as u64,
        }
    }
}

#[tokio::test]
async fn a_drifting_host_expiry_is_refused_instead_of_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(GenerateImage::with_host_context(
        Arc::new(OrderedProvider {
            events: events.clone(),
            image_price: Some(0.01),
            video_price: None,
        }),
        Arc::new(OrderedGate(events.clone())),
        Arc::new(DriftingOperationIds {
            opaque_id: "host-drifting-image",
            calls: AtomicUsize::new(0),
        }),
        operation_journal(),
    ));
    let args = serde_json::json!({"prompt": "same"});

    let first = tool.execute(&args, dir.path()).await;
    assert!(matches!(first, ToolOutput::Ok { .. }), "{first:?}");
    let second = tool.execute(&args, dir.path()).await;

    let ToolOutput::Error { message } = second else {
        panic!("a moved expiry is a different request, not a replay: {second:?}");
    };
    assert!(
        message.contains("conflicting media identity or expiry"),
        "{message}"
    );
    // Refused at the claim, so the second attempt never reaches the gate
    // or the provider: the two events are the first attempt's.
    assert_eq!(events.lock().unwrap().len(), 2);
}

fn tool() -> GenerateImage {
    GenerateImage::with_host_context(
        Arc::new(FakeImages),
        approving_gate(),
        Arc::new(FixedOperationIds("host-test-image")),
        operation_journal(),
    )
}

#[test]
fn schema_is_mutating_and_named() {
    let schema = tool().schema();
    assert_eq!(schema.name, "generate_image");
    assert!(!schema.read_only);
}

#[tokio::test]
async fn image_without_a_host_gate_is_denied_before_submission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = Arc::new(CountingImages {
        submits: AtomicUsize::new(0),
        manifest_blocker: None,
    });
    let out = GenerateImage::new(provider.clone())
        .execute(&serde_json::json!({"prompt": "a star"}), dir.path())
        .await;
    match out {
        ToolOutput::Error { message } => {
            assert!(message.contains("host approval"), "{message}");
        }
        ToolOutput::Ok { content } => panic!("the host gate must deny: {content}"),
    }
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn media_approval_requires_a_process_free_registry() {
    let provider = Arc::new(CountingImages {
        submits: AtomicUsize::new(0),
        manifest_blocker: None,
    });
    let gate = Arc::new(ApproveSpendGate {
        calls: AtomicUsize::new(0),
    });
    let backend = || MediaBackend {
        image: provider.clone(),
        video: None,
    };

    let denied_root = tempfile::tempdir().expect("tempdir");
    let denied = crate::registry::ToolRegistry::with_backends_and_options(
        denied_root.path().to_path_buf(),
        None,
        Some(backend()),
        crate::registry::RegistryOptions {
            media_requires_host_approval: false,
            media_spend_gate: Some(gate.clone()),
            media_operation_ids: Some(Arc::new(FixedOperationIds("host-denied-image"))),
            media_operation_journal: Some(operation_journal()),
            ..Default::default()
        },
    );
    let denied_out = denied
        .execute("generate_image", &serde_json::json!({"prompt": "a star"}))
        .await;
    assert!(denied_out.is_error(), "{denied_out:?}");
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);

    let missing_id_root = tempfile::tempdir().expect("tempdir");
    let missing_id = crate::registry::ToolRegistry::with_backends_and_options(
        missing_id_root.path().to_path_buf(),
        None,
        Some(backend()),
        crate::registry::RegistryOptions {
            media_requires_host_approval: true,
            media_spend_gate: Some(gate.clone()),
            ..Default::default()
        },
    );
    let missing_id_out = missing_id
        .execute("generate_image", &serde_json::json!({"prompt": "a star"}))
        .await;
    assert!(missing_id_out.is_error(), "{missing_id_out:?}");
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);

    let missing_journal = crate::registry::ToolRegistry::with_backends_and_options(
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
        Some(backend()),
        crate::registry::RegistryOptions {
            media_requires_host_approval: true,
            media_spend_gate: Some(gate.clone()),
            media_operation_ids: Some(Arc::new(FixedOperationIds("host-missing-journal"))),
            ..Default::default()
        },
    );
    let missing_journal_out = missing_journal
        .execute("generate_image", &serde_json::json!({"prompt": "a star"}))
        .await;
    // Since #785 an incomplete host context no longer registers a
    // deny-only tool at all — the refusal surfaces as the tool's absence
    // rather than a per-call denial naming the missing piece.
    assert!(
        format!("{missing_journal_out:?}").contains("unknown tool"),
        "{missing_journal_out:?}"
    );
    assert!(
        !missing_journal
            .schemas()
            .iter()
            .any(|s| s.name == "generate_image"),
        "a config without an operation journal must not surface generate_image"
    );
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);

    let unconfined_root = tempfile::tempdir().expect("tempdir");
    let unconfined = crate::registry::ToolRegistry::with_backends_and_options(
        unconfined_root.path().to_path_buf(),
        None,
        Some(backend()),
        crate::registry::RegistryOptions {
            media_requires_host_approval: true,
            media_spend_gate: Some(gate.clone()),
            media_operation_ids: Some(Arc::new(FixedOperationIds("host-approved-image"))),
            media_operation_journal: Some(operation_journal()),
            ..Default::default()
        },
    );
    let unconfined_out = unconfined
        .execute("generate_image", &serde_json::json!({"prompt": "a star"}))
        .await;

    assert!(unconfined_out.is_error(), "{unconfined_out:?}");
    let unconfined_names: Vec<_> = unconfined
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    for expected in ["build_project", "run_tests", "start_process"] {
        assert!(
            unconfined_names.iter().any(|name| name == expected),
            "ordinary registry lost {expected}: {unconfined_names:?}"
        );
    }
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);

    let isolated_root = tempfile::tempdir().expect("tempdir");
    let isolated = crate::registry::ToolRegistry::with_backends_and_options(
        isolated_root.path().to_path_buf(),
        Some(crate::issues::IssueBackend::GitHub),
        Some(backend()),
        crate::registry::RegistryOptions {
            media_requires_host_approval: true,
            media_spend_gate: Some(gate.clone()),
            media_operation_ids: Some(Arc::new(FixedOperationIds("host-isolated-image"))),
            media_operation_journal: Some(operation_journal()),
            media_host_data_isolation: Some(HostDataIsolation::ProcessFree),
            ..Default::default()
        },
    );
    let isolated_names: Vec<_> = isolated
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    for forbidden in [
        "bash",
        "verify_done",
        "build_project",
        "run_tests",
        "run_lint",
        "format_code",
        "start_process",
        "read_output",
        "send_stdin",
        "stop_process",
        "run_script",
        "repo_status",
        "repo_commit",
        "repo_push",
        "repo_pull",
        "repo_rollback",
        "ci_status",
        "screenshot",
        "task_assign",
        "gather_context",
        "grep",
        "glob",
        "explorations",
        "save_exploration",
        "create_issue",
        "start_work_on_issue",
    ] {
        assert!(
            !isolated_names.iter().any(|name| name == forbidden),
            "process-free registry exposed {forbidden}: {isolated_names:?}"
        );
    }
    let isolated_out = isolated
        .execute("generate_image", &serde_json::json!({"prompt": "a star"}))
        .await;

    assert!(!isolated_out.is_error(), "{isolated_out:?}");
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn generates_and_persists_under_stella_artifacts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = tool()
        .execute(
            &serde_json::json!({"prompt": "a star", "label": "star"}),
            dir.path(),
        )
        .await;
    match out {
        ToolOutput::Ok { content } => {
            assert!(content.contains("star"), "labels the artifact: {content}");
            assert!(content.contains("fake-image-1"), "names the model");
            // The artifact actually landed inside .stella/artifacts/.
            let artifacts = dir.path().join(".stella").join("artifacts");
            let count = std::fs::read_dir(&artifacts)
                .map(|d| d.filter_map(Result::ok).count())
                .unwrap_or(0);
            assert!(count >= 1, "one artifact on disk");
        }
        ToolOutput::Error { message } => panic!("expected success: {message}"),
    }
}

#[tokio::test]
async fn refusal_and_bad_size_and_missing_prompt_are_named_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let refused = tool()
        .execute(&serde_json::json!({"prompt": "refuse me"}), dir.path())
        .await;
    match refused {
        ToolOutput::Error { message } => assert!(message.contains("content policy")),
        ToolOutput::Ok { .. } => panic!("refusal must surface as an error"),
    }
    let bad_size = tool()
        .execute(
            &serde_json::json!({"prompt": "ok", "size": "huge"}),
            dir.path(),
        )
        .await;
    assert!(matches!(bad_size, ToolOutput::Error { .. }));
    let no_prompt = tool().execute(&serde_json::json!({}), dir.path()).await;
    assert!(matches!(no_prompt, ToolOutput::Error { .. }));
}

// video

/// A deterministic video provider: fixed job handle, scripted poll
/// state, no network — proves the tool pair drives the gate, the job
/// store, and the artifact store, not the wire.
struct FakeVideo {
    usd_per_second: Option<f64>,
    state: std::sync::Mutex<MediaJobState>,
    submits: AtomicUsize,
    journal_blocker: Option<std::path::PathBuf>,
}

impl FakeVideo {
    fn new(usd_per_second: Option<f64>, state: MediaJobState) -> Arc<Self> {
        Arc::new(Self {
            usd_per_second,
            state: std::sync::Mutex::new(state),
            submits: AtomicUsize::new(0),
            journal_blocker: None,
        })
    }

    fn with_journal_blocker(blocker: std::path::PathBuf) -> Arc<Self> {
        Arc::new(Self {
            usd_per_second: Some(0.2),
            state: std::sync::Mutex::new(MediaJobState::Running),
            submits: AtomicUsize::new(0),
            journal_blocker: Some(blocker),
        })
    }
}

#[async_trait]
impl MediaProvider for FakeVideo {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: "fake".into(),
            video: true,
            video_usd_per_second: self.usd_per_second,
            ..Default::default()
        }
    }
    async fn generate_image(&self, _req: ImageRequest) -> Result<MediaArtifact, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
    async fn generate_video(&self, req: VideoRequest) -> Result<MediaJob, MediaError> {
        let attempt = self.submits.fetch_add(1, Ordering::SeqCst);
        if attempt == 0
            && let Some(blocker) = &self.journal_blocker
        {
            std::fs::create_dir_all(blocker).unwrap();
        }
        let estimated_cost_usd = self
            .capabilities()
            .estimate_video(req.duration_secs)
            .map(|e| e.estimated_usd)
            .unwrap_or(0.0);
        Ok(MediaJob {
            artifact_id: "med_fake".into(),
            provider_id: "fake".into(),
            provider_job_id: format!("vid-{}", attempt + 1),
            kind: MediaKind::Video,
            model: "fake-video-1".into(),
            estimated_cost_usd,
            submitted_at: unix_now(),
            label: req.label,
        })
    }
    async fn poll_video(&self, job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        let state = self.state.lock().expect("lock").clone();
        let artifact = matches!(state, MediaJobState::Succeeded).then(|| MediaArtifact {
            kind: MediaKind::Video,
            bytes: b"mp4-bytes".to_vec(),
            extension: "mp4".into(),
            label: job.label.clone(),
            model: job.model.clone(),
            cost_usd: job.estimated_cost_usd,
        });
        Ok(MediaJobStatus {
            state,
            progress: None,
            artifact,
        })
    }
}

fn job_store(root: &std::path::Path) -> JobStore {
    JobStore::open(root.join(".stella").join("artifacts"))
}

/// Submit through an approving host gate so poll tests exercise the real
/// persisted handle.
async fn submit(fake: &Arc<FakeVideo>, root: &std::path::Path) -> Arc<dyn MediaOperationJournal> {
    let journal = operation_journal();
    let out = GenerateVideo::with_host_context(
        fake.clone(),
        approving_gate(),
        Arc::new(FixedOperationIds("host-submit-video")),
        journal.clone(),
    )
    .execute(&serde_json::json!({"prompt": "a teaser"}), root)
    .await;
    assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
    journal
}

#[test]
fn video_and_svg_schemas_are_mutating_and_named() {
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    for (schema, name) in [
        (
            GenerateVideo::with_host_context(
                fake.clone(),
                approving_gate(),
                Arc::new(FixedOperationIds("host-schema-video")),
                operation_journal(),
            )
            .schema(),
            "generate_video",
        ),
        (PollVideo::new(fake).schema(), "poll_video"),
        (GenerateSvg.schema(), "generate_svg"),
    ] {
        assert_eq!(schema.name, name);
        assert!(!schema.read_only);
        assert!(
            schema.input_schema["properties"]
                .get("confirm_spend")
                .is_none(),
            "model schemas must not expose spend authority"
        );
    }
}

#[tokio::test]
async fn video_without_a_host_gate_is_denied_before_submission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    let out = GenerateVideo::new(fake.clone())
        .execute(&serde_json::json!({"prompt": "a teaser"}), dir.path())
        .await;
    match out {
        ToolOutput::Error { message } => {
            assert!(message.contains("host approval"), "{message}");
        }
        ToolOutput::Ok { content } => panic!("the gate must deny: {content}"),
    }
    assert_eq!(
        fake.submits.load(Ordering::SeqCst),
        0,
        "a denied job must never reach the provider"
    );
    assert!(job_store(dir.path()).list().unwrap().is_empty());
}

#[tokio::test]
async fn model_controlled_confirm_spend_cannot_authorize_video() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    let out = GenerateVideo::new(fake.clone())
        .execute(
            &serde_json::json!({
                "prompt": "a teaser", "duration_secs": 10,
                "confirm_spend": true, "label": "teaser"
            }),
            dir.path(),
        )
        .await;
    match out {
        ToolOutput::Error { message } => {
            assert!(message.contains("host approval"), "{message}");
        }
        ToolOutput::Ok { content } => {
            panic!("model-controlled consent must not pass: {content}")
        }
    }
    assert_eq!(fake.submits.load(Ordering::SeqCst), 0);
    assert!(job_store(dir.path()).list().unwrap().is_empty());
}

#[tokio::test]
async fn video_without_a_rate_card_still_requires_host_approval() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(None, MediaJobState::Running);
    let out = GenerateVideo::new(fake.clone())
        .execute(&serde_json::json!({"prompt": "a teaser"}), dir.path())
        .await;
    assert!(matches!(out, ToolOutput::Error { .. }), "{out:?}");
    assert_eq!(fake.submits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn video_completion_journal_failure_is_reconciliation_required_and_retry_safe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir
        .path()
        .join(".stella")
        .join("artifacts")
        .join("jobs.json.lock");
    let fake = FakeVideo::with_journal_blocker(blocker.clone());
    let tool = GenerateVideo::with_host_context(
        fake.clone(),
        approving_gate(),
        Arc::new(FixedOperationIds("host-retry-video")),
        operation_journal(),
    );
    let input = serde_json::json!({"prompt": "a teaser"});

    let first = tool.execute(&input, dir.path()).await;
    let ToolOutput::Error { message } = first else {
        panic!("persistence failure must be terminal: {first:?}");
    };
    assert!(message.contains("reconciliation_required"), "{message}");
    assert!(!message.contains("a teaser"), "{message}");
    assert_eq!(fake.submits.load(Ordering::SeqCst), 1);

    std::fs::remove_dir(&blocker).unwrap();
    let retry = tool.execute(&input, dir.path()).await;
    let ToolOutput::Error { message } = retry else {
        panic!("an ambiguous pending operation must not resubmit: {retry:?}");
    };
    assert!(message.contains("reconciliation_required"), "{message}");
    assert_eq!(fake.submits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn image_artifact_failure_is_reconciliation_required_and_retry_safe() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory where the manifest file belongs: `entries()` cannot
    // read it, so the save fails at the manifest step. Blocking the temp
    // file instead would not work — the temp name now carries pid and a
    // counter, precisely so two writers cannot collide on one path.
    let blocker = dir
        .path()
        .join(".stella")
        .join("artifacts")
        .join("manifest.json");
    let provider = Arc::new(CountingImages {
        submits: AtomicUsize::new(0),
        manifest_blocker: Some(blocker.clone()),
    });
    let tool = GenerateImage::with_host_context(
        provider.clone(),
        approving_gate(),
        Arc::new(FixedOperationIds("host-retry-image")),
        operation_journal(),
    );
    let input = serde_json::json!({"prompt": "a star"});

    let first = tool.execute(&input, dir.path()).await;
    let ToolOutput::Error { message } = first else {
        panic!("artifact failure must be terminal: {first:?}");
    };
    assert!(message.contains("reconciliation_required"), "{message}");
    assert!(!message.contains("a star"), "{message}");
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);

    std::fs::remove_dir(&blocker).unwrap();
    let retry = tool.execute(&input, dir.path()).await;
    assert!(matches!(retry, ToolOutput::Error { .. }), "{retry:?}");
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn distinct_host_operation_ids_allow_intentional_identical_video_generations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    let input = serde_json::json!({"prompt": "same teaser"});
    for operation_id in ["host-intent-one", "host-intent-two"] {
        let out = GenerateVideo::with_host_context(
            fake.clone(),
            approving_gate(),
            Arc::new(FixedOperationIds(operation_id)),
            operation_journal(),
        )
        .execute(&input, dir.path())
        .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
    }
    assert_eq!(fake.submits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn poll_running_keeps_the_job_persisted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    submit(&fake, dir.path()).await;
    let out = PollVideo::new(fake)
        .execute(&serde_json::json!({"job_id": "vid-1"}), dir.path())
        .await;
    match out {
        ToolOutput::Ok { content } => assert!(content.contains("running"), "{content}"),
        ToolOutput::Error { message } => panic!("expected running status: {message}"),
    }
    assert!(
        job_store(dir.path()).get("vid-1").unwrap().is_some(),
        "a non-terminal poll must keep the handle"
    );
}

#[tokio::test]
async fn poll_success_persists_the_video_and_forgets_the_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Succeeded);
    let journal = submit(&fake, dir.path()).await;
    let out = PollVideo::with_operation_journal(fake.clone(), journal.clone())
        .execute(&serde_json::json!({"job_id": "vid-1"}), dir.path())
        .await;
    match out {
        ToolOutput::Ok { content } => {
            // Saved under the artifact id assigned at submit.
            assert!(content.contains("med_fake.mp4"), "{content}");
        }
        ToolOutput::Error { message } => panic!("expected success: {message}"),
    }
    let file = dir
        .path()
        .join(".stella")
        .join("artifacts")
        .join("med_fake.mp4");
    assert_eq!(std::fs::read(&file).unwrap(), b"mp4-bytes");
    assert!(
        job_store(dir.path()).get("vid-1").unwrap().is_none(),
        "a terminal, persisted job is forgotten"
    );
    let replay = GenerateVideo::with_host_context(
        fake.clone(),
        approving_gate(),
        Arc::new(FixedOperationIds("host-submit-video")),
        journal,
    )
    .execute(&serde_json::json!({"prompt": "a teaser"}), dir.path())
    .await;
    let ToolOutput::Ok { content } = replay else {
        panic!("completed operation must replay: {replay:?}");
    };
    assert!(content.contains("already completed"), "{content}");
    assert_eq!(fake.submits.load(Ordering::SeqCst), 1);
    // A second poll of the forgotten job is a named error, never a
    // stale replay.
    let again = PollVideo::new(fake)
        .execute(&serde_json::json!({"job_id": "vid-1"}), dir.path())
        .await;
    assert!(matches!(again, ToolOutput::Error { .. }), "{again:?}");
}

#[tokio::test]
async fn poll_does_not_report_success_when_job_cleanup_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Succeeded);
    let journal = submit(&fake, dir.path()).await;
    let lock = dir.path().join(".stella/artifacts/jobs.json.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();

    let out = PollVideo::with_operation_journal(fake.clone(), journal.clone())
        .execute(&serde_json::json!({"job_id": "vid-1"}), dir.path())
        .await;

    match out {
        ToolOutput::Error { message } => {
            assert!(message.contains("reconciliation_required"), "{message}");
        }
        ToolOutput::Ok { content } => panic!("stale handle reported success: {content}"),
    }
    assert!(job_store(dir.path()).get("vid-1").unwrap().is_some());
    let replay = GenerateVideo::with_host_context(
        fake.clone(),
        approving_gate(),
        Arc::new(FixedOperationIds("host-submit-video")),
        journal,
    )
    .execute(&serde_json::json!({"prompt": "a teaser"}), dir.path())
    .await;
    let ToolOutput::Ok { content } = replay else {
        panic!("completion must survive cleanup failure: {replay:?}");
    };
    assert!(content.contains("already completed"), "{content}");
    assert_eq!(fake.submits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn poll_failure_reports_the_reason_and_forgets_the_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(
        Some(0.2),
        MediaJobState::Failed {
            reason: "provider purged the job".into(),
        },
    );
    submit(&fake, dir.path()).await;
    let out = PollVideo::new(fake)
        .execute(&serde_json::json!({"job_id": "vid-1"}), dir.path())
        .await;
    match out {
        ToolOutput::Error { message } => assert!(message.contains("purged"), "{message}"),
        ToolOutput::Ok { content } => panic!("a failed job must error: {content}"),
    }
    assert!(job_store(dir.path()).get("vid-1").unwrap().is_none());
}

#[tokio::test]
async fn poll_unknown_job_and_missing_id_are_named_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = FakeVideo::new(Some(0.2), MediaJobState::Running);
    let unknown = PollVideo::new(fake.clone())
        .execute(&serde_json::json!({"job_id": "ghost"}), dir.path())
        .await;
    match unknown {
        ToolOutput::Error { message } => assert!(message.contains("ghost"), "{message}"),
        ToolOutput::Ok { content } => panic!("unknown job must error: {content}"),
    }
    let no_id = PollVideo::new(fake)
        .execute(&serde_json::json!({}), dir.path())
        .await;
    assert!(matches!(no_id, ToolOutput::Error { .. }));
}

// svg

#[tokio::test]
async fn svg_is_sanitized_and_persisted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hostile = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><script>x()</script><rect/></svg>"#;
    let out = GenerateSvg
        .execute(
            &serde_json::json!({"svg": hostile, "label": "icon"}),
            dir.path(),
        )
        .await;
    match out {
        ToolOutput::Ok { content } => {
            assert!(content.contains(".svg"), "{content}");
            assert!(
                content.contains("stripped"),
                "reports sanitization: {content}"
            );
            // The persisted artifact is the sanitized text, not the input.
            let artifacts = dir.path().join(".stella").join("artifacts");
            let svg_file = std::fs::read_dir(&artifacts)
                .unwrap()
                .filter_map(Result::ok)
                .find(|e| e.path().extension().is_some_and(|x| x == "svg"))
                .expect("svg on disk");
            let text = std::fs::read_to_string(svg_file.path()).unwrap();
            assert!(!text.contains("script"), "{text}");
            assert!(text.contains("<rect"), "{text}");
        }
        ToolOutput::Error { message } => panic!("expected success: {message}"),
    }
}

#[tokio::test]
async fn malformed_svg_is_a_repairable_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = GenerateSvg
        .execute(&serde_json::json!({"svg": "<svg><rect></svg>"}), dir.path())
        .await;
    match out {
        // The line/col detail is what lets the model repair and retry.
        ToolOutput::Error { message } => assert!(message.contains("parse error"), "{message}"),
        ToolOutput::Ok { content } => panic!("malformed SVG must error: {content}"),
    }
    let no_svg = GenerateSvg
        .execute(&serde_json::json!({}), dir.path())
        .await;
    assert!(matches!(no_svg, ToolOutput::Error { .. }));
}
