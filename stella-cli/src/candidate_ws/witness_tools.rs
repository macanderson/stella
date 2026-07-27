//! Capability-minimal tool surface for candidate-local witness authoring.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use stella_core::ToolExecutor;
use stella_pipeline::ports::WorkspaceError;
use stella_protocol::{ToolOutput, ToolSchema};

/// Candidate-root executor built specifically for witness authoring. Reads
/// stay in the snapshot; the sole mutation atomically creates one new test.
pub(super) struct WitnessToolExecutor {
    root: PathBuf,
    reads: Arc<dyn ToolExecutor>,
    created_path: Mutex<Option<String>>,
}

impl WitnessToolExecutor {
    pub(super) fn new(root: PathBuf, reads: Arc<dyn ToolExecutor>) -> Self {
        Self {
            root,
            reads,
            created_path: Mutex::new(None),
        }
    }

    fn denied(name: &str, reason: impl std::fmt::Display) -> ToolOutput {
        ToolOutput::Error {
            message: format!("`{name}` is not available to the witness author: {reason}"),
        }
    }

    fn create_test(&self, input: &serde_json::Value) -> ToolOutput {
        let Some(raw_path) = input.get("path").and_then(serde_json::Value::as_str) else {
            return Self::denied(
                "create_witness_test",
                "a workspace-relative `path` is required",
            );
        };
        let Some(path) = normalized_candidate_path(raw_path) else {
            return Self::denied(
                "create_witness_test",
                "the path must stay within the candidate root",
            );
        };
        if !stella_pipeline::witness::is_witness_test_path(&path) {
            return Self::denied(
                "create_witness_test",
                "the created artifact must be a recognized test file",
            );
        }
        let Some(content) = input.get("content").and_then(serde_json::Value::as_str) else {
            return Self::denied("create_witness_test", "string `content` is required");
        };

        let mut claimed = self
            .created_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if claimed.is_some() {
            return Self::denied("create_witness_test", "only one create attempt may succeed");
        }
        let Ok(root) = self.root.canonicalize() else {
            return Self::denied("create_witness_test", "the candidate root is unavailable");
        };
        let joined = root.join(&path);
        let Some(parent) = joined.parent() else {
            return Self::denied(
                "create_witness_test",
                "the artifact has no parent directory",
            );
        };
        let Ok(parent) = parent.canonicalize() else {
            return Self::denied(
                "create_witness_test",
                "the parent directory must already exist",
            );
        };
        if !parent.starts_with(&root) {
            return Self::denied(
                "create_witness_test",
                "the canonical parent escapes the candidate root",
            );
        }
        let Some(name) = joined.file_name() else {
            return Self::denied("create_witness_test", "the artifact has no file name");
        };
        let target = parent.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = match options.open(&target) {
            Ok(file) => file,
            Err(error) => {
                return Self::denied(
                    "create_witness_test",
                    format!("exclusive file creation failed: {error}"),
                );
            }
        };
        if let Err(error) = file.write_all(content.as_bytes()) {
            drop(file);
            let _ = std::fs::remove_file(&target);
            return Self::denied(
                "create_witness_test",
                format!("writing the new test failed: {error}"),
            );
        }
        *claimed = Some(path.clone());
        ToolOutput::Ok {
            content: format!("created witness test `{path}`"),
        }
    }
}

#[async_trait]
impl ToolExecutor for WitnessToolExecutor {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<_> = self
            .reads
            .schemas()
            .into_iter()
            .filter(|schema| schema.name == "read_file")
            .collect();
        schemas.push(ToolSchema {
            name: "create_witness_test".into(),
            description: "Atomically create one previously absent test file inside the candidate. Existing files and additional creations are refused.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "New test file path relative to the candidate root" },
                    "content": { "type": "string", "description": "Complete test source" }
                },
                "required": ["path", "content"]
            }),
            read_only: false,
        });
        schemas
    }

    async fn execute(&self, name: &str, input: &serde_json::Value) -> ToolOutput {
        match name {
            "read_file" => {
                let Some(raw_path) = input.get("path").and_then(serde_json::Value::as_str) else {
                    return Self::denied(name, "a workspace-relative `path` is required");
                };
                let Some(path) = normalized_candidate_path(raw_path) else {
                    return Self::denied(name, "the path must stay within the candidate root");
                };
                if is_credential_path(&path) {
                    return Self::denied(name, "credential and private-state paths are excluded");
                }
                self.reads.execute(name, input).await
            }
            "create_witness_test" => self.create_test(input),
            _ => Self::denied(
                name,
                "only candidate reads and atomic witness creation are allowed",
            ),
        }
    }
}

pub(super) fn normalized_candidate_path(raw: &str) -> Option<String> {
    let slash = raw.replace('\\', "/");
    if slash.is_empty() || slash.as_bytes().get(1) == Some(&b':') || Path::new(&slash).is_absolute()
    {
        return None;
    }
    let mut parts = Vec::new();
    for component in Path::new(&slash).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn is_credential_path(path: &str) -> bool {
    let components: Vec<_> = path
        .split('/')
        .map(|component| component.to_ascii_lowercase())
        .collect();
    components
        .windows(2)
        .any(|pair| pair == [".stella", "private"])
        || components.iter().any(|component| {
            matches!(
                component.as_str(),
                ".git" | ".ssh" | ".aws" | ".azure" | ".config" | ".kube"
            ) || component == ".env"
                || component.starts_with(".env.")
                || matches!(
                    component.as_str(),
                    "credentials.json"
                        | "creds.json"
                        | ".netrc"
                        | ".npmrc"
                        | ".pypirc"
                        | "id_rsa"
                        | "id_ed25519"
                )
                || component.ends_with(".pem")
                || component.ends_with(".key")
        })
}

/// Copy one accepted witness artifact from the authoring snapshot into
/// this workspace, create-only and following no link on either side.
///
/// The source is read with `O_NOFOLLOW` and the destination opened
/// `create_new` + `O_NOFOLLOW`, so neither end can be redirected by a
/// symlink planted between acceptance and this copy. Bytes are moved
/// rather than the file being hard-linked or reflinked: the pipeline
/// re-fingerprints the destination immediately afterwards and pins tamper
/// exclusion to it, and a shared inode would let the authoring snapshot's
/// teardown change what the candidate is about to run.
///
/// Only the file is created. A missing parent directory is an error rather
/// than an implicit `create_dir_all`: the artifact path was validated as a
/// recognized test path inside a *tree that already had that directory*,
/// so a parent that is absent here means the two trees disagree about
/// layout, and inventing directories in the candidate would paper over it.
pub(super) async fn graft(
    candidate_root: &str,
    workspace_dir: &Path,
    source_root: &str,
    path: &str,
) -> Result<(), WorkspaceError> {
    let fail = |reason: String| WorkspaceError::Graft {
        reason,
        path: path.to_string(),
        workspace: workspace_dir.display().to_string(),
    };
    let Some(rel) = normalized_candidate_path(path) else {
        return Err(fail("the path must stay within the candidate root".into()));
    };
    let source = Path::new(source_root).join(&rel);
    let bytes = read_nofollow(&source)
        .await
        .map_err(|e| fail(format!("could not read the authored artifact: {e}")))?;

    let root = Path::new(candidate_root)
        .canonicalize()
        .map_err(|e| fail(format!("the candidate root is unavailable: {e}")))?;
    let joined = root.join(&rel);
    let parent = joined
        .parent()
        .ok_or_else(|| fail("the artifact has no parent directory".into()))?
        .canonicalize()
        .map_err(|e| fail(format!("the parent directory must already exist: {e}")))?;
    if !parent.starts_with(&root) {
        return Err(fail(
            "the canonical parent escapes the candidate root".into(),
        ));
    }
    let name = joined
        .file_name()
        .ok_or_else(|| fail("the artifact has no file name".into()))?;
    let target = parent.join(name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(&target).map_err(|e| {
        // `AlreadyExists` is the interesting one: the worker wrote its own
        // file at the artifact's path. Overwriting would delete real work
        // to make room for scaffolding, so the graft fails and the run
        // finishes on the unauthored ladder.
        fail(format!("exclusive file creation failed: {e}"))
    })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            drop(file);
            let _ = std::fs::remove_file(&target);
            fail(format!("writing the grafted artifact failed: {e}"))
        })
}

/// Read a file's bytes without following a final-component symlink.
///
/// Used for the one file that crosses between candidate workspaces. The
/// pipeline has already accepted this path as a regular, singly-linked file in
/// the tree that authored it; opening `O_NOFOLLOW` here keeps that acceptance
/// meaningful by refusing to resolve a link swapped in afterwards.
async fn read_nofollow(src: &Path) -> std::io::Result<Vec<u8>> {
    let src = src.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&src)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_tools::{RegistryOptions, ToolRegistry};

    async fn witness_executor(root: &Path) -> WitnessToolExecutor {
        let registry: Arc<dyn ToolExecutor> = Arc::new(
            ToolRegistry::new_detected(root.to_path_buf(), RegistryOptions::default()).await,
        );
        WitnessToolExecutor::new(root.to_path_buf(), registry)
    }

    #[tokio::test]
    async fn has_no_general_or_external_capabilities() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("src.rs"), "source").unwrap();
        std::fs::write(root.path().join(".env.local"), "secret").unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/config"), "credential-helper").unwrap();
        let tools = witness_executor(root.path()).await;

        let names: Vec<_> = tools
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect();
        assert_eq!(names, vec!["read_file", "create_witness_test"]);
        for denied in [
            "write_file",
            "edit_file",
            "bash",
            "run_tests",
            "mcp_external",
            "custom_script",
            "web_search",
            "get_issue",
            "generate_image",
            "save_memory",
        ] {
            assert!(
                tools
                    .execute(denied, &serde_json::json!({}))
                    .await
                    .is_error()
            );
        }
        assert!(
            tools
                .execute("read_file", &serde_json::json!({"path": ".env.local"}))
                .await
                .is_error()
        );
        assert!(
            tools
                .execute("read_file", &serde_json::json!({"path": ".git/config"}))
                .await
                .is_error()
        );
        assert!(matches!(
            tools
                .execute("read_file", &serde_json::json!({"path": "src.rs"}))
                .await,
            ToolOutput::Ok { .. }
        ));
    }

    #[tokio::test]
    async fn exclusive_creation_never_mutates_an_existing_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let existing = root.path().join("tests/existing.rs");
        std::fs::write(&existing, "original").unwrap();
        let tools = witness_executor(root.path()).await;

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/existing.rs", "content": "owned"}),
            )
            .await;
        assert!(output.is_error());
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "original");

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/new_witness.rs", "content": "new"}),
            )
            .await;
        assert!(matches!(output, ToolOutput::Ok { .. }));
        assert_eq!(
            std::fs::read_to_string(root.path().join("tests/new_witness.rs")).unwrap(),
            "new"
        );
        assert!(
            tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/second.rs", "content": "second"}),
                )
                .await
                .is_error()
        );
        assert!(!root.path().join("tests/second.rs").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_terminal_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let target = outside.path().join("target.rs");
        std::fs::write(&target, "outside").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join("tests/witness.rs")).unwrap();
        let tools = witness_executor(root.path()).await;

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/witness.rs", "content": "owned"}),
            )
            .await;
        assert!(output.is_error());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside");
    }

    #[tokio::test]
    async fn concurrent_creates_commit_at_most_one_artifact() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let left_tools = Arc::new(witness_executor(root.path()).await);
        let right_tools = Arc::new(witness_executor(root.path()).await);
        let left = tokio::spawn(async move {
            left_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/raced.rs", "content": "left"}),
                )
                .await
        });
        let right = tokio::spawn(async move {
            right_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/raced.rs", "content": "right"}),
                )
                .await
        });
        let (left, right) = tokio::join!(left, right);
        let outputs = [left.unwrap(), right.unwrap()];
        assert_eq!(
            outputs.iter().filter(|output| !output.is_error()).count(),
            1
        );
        let content = std::fs::read_to_string(root.path().join("tests/raced.rs")).unwrap();
        assert!(matches!(content.as_str(), "left" | "right"));
    }

    #[tokio::test]
    async fn one_executor_allows_only_one_concurrent_creation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let tools = Arc::new(witness_executor(root.path()).await);
        let left_tools = tools.clone();
        let left = tokio::spawn(async move {
            left_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/left.rs", "content": "left"}),
                )
                .await
        });
        let right = tokio::spawn(async move {
            tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/right.rs", "content": "right"}),
                )
                .await
        });

        let (left, right) = tokio::join!(left, right);
        let outputs = [left.unwrap(), right.unwrap()];
        assert_eq!(
            outputs.iter().filter(|output| !output.is_error()).count(),
            1
        );
        let created = ["tests/left.rs", "tests/right.rs"]
            .iter()
            .filter(|path| root.path().join(path).exists())
            .count();
        assert_eq!(created, 1);
    }
}
