use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use stella_media::{
    CostDecision, ImageRequest, MediaArtifact, MediaCapabilities, MediaError, MediaJob,
    MediaJobStatus, MediaKind, MediaProvider, MediaSpendGate, MediaSpendRequest, VideoRequest,
};
use stella_tools::media::{GenerateImage, MediaOperationIdSource};
use stella_tools::registry::Tool;

struct SameHostOperation;

impl MediaOperationIdSource for SameHostOperation {
    fn operation_id(&self) -> String {
        "host-concurrent-retry".into()
    }
}

struct CountingGate(AtomicUsize);

#[async_trait]
impl MediaSpendGate for CountingGate {
    async fn authorize(&self, _request: &MediaSpendRequest) -> CostDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        CostDecision::Approve
    }
}

struct SlowImageProvider(AtomicUsize);

#[async_trait]
impl MediaProvider for SlowImageProvider {
    fn id(&self) -> &str {
        "concurrent-test"
    }

    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: self.id().into(),
            image: true,
            image_usd_each: Some(0.01),
            ..Default::default()
        }
    }

    async fn generate_image(&self, request: ImageRequest) -> Result<MediaArtifact, MediaError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(MediaArtifact {
            kind: MediaKind::Image,
            bytes: b"image".to_vec(),
            extension: "png".into(),
            label: request.label,
            model: "concurrent-test".into(),
            cost_usd: 0.01,
        })
    }

    async fn generate_video(&self, _request: VideoRequest) -> Result<MediaJob, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }

    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
}

#[tokio::test]
async fn concurrent_same_id_claims_authorize_and_submit_once() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(CountingGate(AtomicUsize::new(0)));
    let provider = Arc::new(SlowImageProvider(AtomicUsize::new(0)));
    let tool = GenerateImage::with_host_context(
        provider.clone(),
        gate.clone(),
        Arc::new(SameHostOperation),
    );
    let input = serde_json::json!({"prompt": "same"});

    let (first, second) = tokio::join!(
        tool.execute(&input, dir.path()),
        tool.execute(&input, dir.path())
    );

    assert_eq!(gate.0.load(Ordering::SeqCst), 1);
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        usize::from(first.is_error()) + usize::from(second.is_error()),
        1
    );
}
