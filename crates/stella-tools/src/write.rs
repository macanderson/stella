//! `write_file` — create or overwrite a file, creating parent dirs.
//! Successful writes record the written bytes in the session's read-state
//! ledger (#331): the model knows the content it just produced, so its own
//! write must never be misattributed later as out-of-band drift.
//!
//! # The no-clobber guard (#3738)
//!
//! `write_file` takes the whole file as one argument, so a call against a path
//! that already exists replaces every byte the model did not write — including
//! the bytes it never looked at. That is not a hypothetical: the tool's own
//! description invites "create or overwrite", and a model that has seen a
//! fragment of a file (a ranged read, a `search` hit, a symbol) will
//! confidently hand back the fragment as the whole thing.
//!
//! So an overwrite is refused unless [`ReadLedger::saw_whole_file`] says this
//! session was shown every line of the file. The predicate is deliberately the
//! *coverage* one and not "was it read at all": a partial read is exactly the
//! state that produces a confident truncation. Three consequences, all
//! intended:
//!
//! * **Creating a file needs no read** — there is nothing to clobber, and the
//!   check costs one confined `stat` off the root descriptor already open for
//!   the write.
//! * **A successful write earns its own coverage.** The model authored those
//!   bytes, so it has now seen the file whole; without this a second write to
//!   the same path in one turn would be refused for content the model itself
//!   produced.
//! * **`edit_file` is untouched.** It replaces a needle it had to know to
//!   name, and it is the escape hatch the refusal points at.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::read::ReadLedger;
use crate::registry::Tool;

#[derive(Default)]
pub struct WriteFile {
    ledger: Arc<ReadLedger>,
}

impl WriteFile {
    /// Construct sharing the registry's read-state ledger.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self { ledger }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Create or overwrite a file. Creates parent directories as needed. Overwriting an existing file is refused unless you have already read all of it this session — read_file it in full first, or use edit_file to change part of it in place. To create several files, send them in ONE call with `files`: the whole batch is checked before any of it is written.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": { "type": "string", "description": "Full file content to write" },
                    "files": crate::batch::plural_schema(
                        serde_json::json!({
                            "path": { "type": "string", "description": "File path relative to workspace root" },
                            "content": { "type": "string", "description": "Full file content to write" }
                        }),
                        &["path", "content"],
                        "Several files written as ONE all-or-nothing change — every path is \
                         scope-checked and every overwrite guard is asked before any byte is \
                         written.",
                    ),
                    "reason": { "type": "string", "description": "Why you are creating/overwriting this file — recorded in the session's file-touch audit log" },
                    "storage_intent": { "type": "string", "description": "Only when creating a database table/column that the storage gate flagged as similar to an existing one: one sentence of purpose plus why the existing objects don't fit. Recorded in stella.storage.toml." }
                },
                "required": []
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        if crate::batch::is_plural(input, FILES_KEY) {
            return self.write_batch(input, ctx).await;
        }
        self.write_one(input, ctx).await
    }
}

/// The plural key: several files written as one all-or-nothing change.
const FILES_KEY: &str = "files";

/// One file and its contents — the unit both spellings reduce to.
struct WriteTarget {
    path: String,
    content: String,
}

/// Parse one target. Shared by the single form and by every element of
/// `files`, so the two spellings cannot drift into two meanings.
fn write_target(value: &Value) -> Result<WriteTarget, crate::input::InputError> {
    Ok(WriteTarget {
        path: crate::input::required_str(value, "path")?.to_string(),
        content: crate::input::required_str(value, "content")?.to_string(),
    })
}

/// What is at `path` before this call replaces it — `None` when nothing is.
///
/// Read through the descriptor the write is about to walk (#938), so the old
/// side of the diff and the file being overwritten are one file. The bytes are
/// decoded lossily: the new side is a `String` by construction, and a diff
/// over a lossy old side still states every line that changed, where refusing
/// to diff would state nothing.
fn before(handle: &crate::rootfd::RootHandle, path: &str) -> Option<String> {
    if handle.stat(path).is_err() {
        return None;
    }
    handle
        .read(path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

impl WriteFile {
    /// Write several files as one change.
    ///
    /// Every path is scope-resolved and every no-clobber guard is asked
    /// **before** any byte is written, so a batch that would be refused on its
    /// third file does not leave the first two on disk. Rolling a write back is
    /// not possible once the old bytes are gone; validating first is, and it is
    /// the same guarantee for the case that actually happens.
    async fn write_batch(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let targets = match crate::batch::targets(input, FILES_KEY, "path", write_target) {
            Ok(targets) => targets,
            Err(err) => return ToolOutput::from(err),
        };

        // Pass one: resolve and check. Nothing is written in this loop.
        let mut planned = Vec::with_capacity(targets.len());
        for (index, target) in targets.iter().enumerate() {
            let (scope_root, path) = match ctx.resolve_for_write(&target.path) {
                Ok(resolved) => resolved,
                Err(refusal) => {
                    return ToolOutput::error(format!(
                        "`{FILES_KEY}`[{index}] (`{}`): {refusal} — nothing was written",
                        target.path
                    ));
                }
            };
            let handle = match crate::rootfd::RootHandle::open(&scope_root) {
                Ok(handle) => Arc::new(handle),
                Err(e) => return ToolOutput::error(format!("cannot open workspace root: {e}")),
            };
            if handle.stat(&path).is_ok() && !self.ledger.saw_whole_file(root, &path) {
                return ToolOutput::error(format!(
                    "`{FILES_KEY}`[{index}]: refusing to overwrite `{path}`: this session has \
                     not been shown the whole file, and `write_file` replaces all of it. Read \
                     it first (`read_file` with no offset/limit), then write — or use \
                     `edit_file` to change part of it in place. Nothing was written."
                ));
            }
            planned.push((handle, scope_root, path, target.content.as_str()));
        }

        // Pass two: every check passed, so commit.
        let mut report = Vec::with_capacity(planned.len());
        let mut changes = Vec::with_capacity(planned.len());
        for (handle, scope_root, path, content) in planned {
            let previous = before(&handle, &path);
            match crate::durable_write::write_file_durably_at(
                handle,
                path.clone(),
                content.as_bytes().to_vec(),
                true,
            )
            .await
            {
                Ok(()) => {
                    self.ledger.record_known(root, &path, content);
                    self.ledger.record_coverage(root, &path, true);
                    report.push(format!("{path} — {} bytes", content.len()));
                    changes.push(crate::own_change::own_change(
                        &crate::own_change::workspace_path(root, &scope_root, &path),
                        previous.as_deref(),
                        content,
                    ));
                }
                Err(e) => return ToolOutput::error(format!("failed to write `{path}`: {e}")),
            }
        }
        crate::own_change::attach(
            ToolOutput::ok(format!(
                "wrote {} file(s):\n{}",
                report.len(),
                report.join("\n")
            )),
            &changes,
        )
    }

    async fn write_one(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let path = match crate::input::required_str(input, "path") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let content = match crate::input::required_str(input, "content") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };

        // Which directory this write is allowed to land in, and where inside
        // it. Decided before anything is opened: a refusal that arrives after
        // the bytes are on disk is a report, not a boundary.
        let (scope_root, path) = match ctx.resolve_for_write(path) {
            Ok(resolved) => resolved,
            Err(refusal) => return ToolOutput::error(refusal.to_string()),
        };
        let path = path.as_str();

        // The root is opened once and held; every component of `path` is then
        // opened relative to the one above it, and the parent directories are
        // created *during* that walk. There is no `create_dir_all` here any
        // more, because a `create_dir_all` re-resolves the whole prefix from a
        // string — the window #938 is about.
        let handle = match crate::rootfd::RootHandle::open(&scope_root) {
            Ok(handle) => Arc::new(handle),
            Err(e) => {
                return ToolOutput::error(format!("cannot open workspace root: {e}"));
            }
        };

        // The no-clobber guard (module header). Asked through the descriptor
        // already open for the write rather than by path, so the existence
        // answer and the bytes that follow it name the same file; and asked
        // *before* the write, because a refusal that arrives after the content
        // is gone is a report, not a boundary. A `stat` that fails for any
        // reason — absent, unreadable, an escape — is not evidence that
        // something is there to destroy, and the write below answers it
        // properly.
        if handle.stat(path).is_ok() && !self.ledger.saw_whole_file(root, path) {
            return ToolOutput::error(format!(
                "refusing to overwrite `{path}`: this session has not been shown the whole file, \
                 and `write_file` replaces all of it. Read it first (`read_file` with no \
                 offset/limit), then write — or use `edit_file` to change part of it in place."
            ));
        }

        let previous = before(&handle, path);
        match crate::durable_write::write_file_durably_at(
            handle,
            path.to_string(),
            content.as_bytes().to_vec(),
            true,
        )
        .await
        {
            Ok(()) => {
                self.ledger.record_known(root, path, content);
                // The model authored every byte now on disk, which is the
                // question the guard above asks. Recorded separately from the
                // hash because they are separate facts: `record_known` says
                // what the content is, this says the model has seen all of it.
                self.ledger.record_coverage(root, path, true);
                let bytes = content.len();
                let change = crate::own_change::own_change(
                    &crate::own_change::workspace_path(root, &scope_root, path),
                    previous.as_deref(),
                    content,
                );
                crate::own_change::attach(
                    ToolOutput::ok(format!("wrote {bytes} bytes to {path}")),
                    &[change],
                )
            }
            Err(e) if e.is_escape() => {
                ToolOutput::error(format!("path `{path}` escapes workspace root ({e})"))
            }
            Err(e) => ToolOutput::error(format!("failed to write `{path}`: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare execution context rooted at `root` — every file-tool test
    /// drives the tool through one, since `Tool::execute` takes the
    /// context rather than the bare root path it used to (#3284).
    fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
        crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
    }

    #[tokio::test]
    async fn writes_file_and_creates_parent_dirs() {
        let dir = std::env::temp_dir();
        let path = format!("stella_write_test_{}/sub/dir/file.txt", std::process::id());
        let result = WriteFile::default()
            .execute(
                &serde_json::json!({"path": path, "content": "hello stella"}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("wrote 12 bytes")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        let written = tokio::fs::read_to_string(dir.join(&path)).await.unwrap();
        assert_eq!(written, "hello stella");
        let _ = tokio::fs::remove_dir_all(
            dir.join(format!("stella_write_test_{}", std::process::id())),
        )
        .await;
    }

    #[tokio::test]
    async fn path_escape_returns_error() {
        let dir = std::env::temp_dir();
        let result = WriteFile::default()
            .execute(
                &serde_json::json!({"path": "../../etc/bad", "content": "x"}),
                &cx(&dir),
            )
            .await;
        assert!(result.is_error());
    }

    /// A fresh empty directory unique to one test. The no-clobber tests below
    /// both create and overwrite, so they cannot share the bare temp dir the
    /// single-shot tests above use.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stella_write_guard_{}_{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Witness for #3738: `ReadLedger::saw_whole_file` exists to gate exactly
    /// this call, and before the guard landed the overwrite went through with
    /// the ledger never consulted.
    #[tokio::test]
    async fn overwriting_a_file_the_model_has_not_read_is_refused() {
        let dir = scratch("unread");
        std::fs::write(dir.join("notes.md"), "one\ntwo\nthree\n").expect("seed");

        let result = WriteFile::default()
            .execute(
                &serde_json::json!({"path": "notes.md", "content": "clobbered"}),
                &cx(&dir),
            )
            .await;

        let ToolOutput::Error { message, .. } = result else {
            panic!("expected the no-clobber refusal");
        };
        assert!(message.contains("refusing to overwrite"), "{message}");
        assert!(
            message.contains("read_file"),
            "the refusal names the way out: {message}"
        );
        // The boundary holds before the bytes move, not after.
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).expect("still there"),
            "one\ntwo\nthree\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The coverage the guard wants is a *whole-file* read, and a read that
    /// showed the model one of three lines is the state that produces a
    /// confident truncation.
    #[tokio::test]
    async fn a_ranged_read_does_not_license_an_overwrite() {
        let dir = scratch("ranged");
        std::fs::write(dir.join("notes.md"), "one\ntwo\nthree\n").expect("seed");
        let ledger = Arc::new(ReadLedger::default());

        let read = crate::read::ReadFile::with_ledger(ledger.clone())
            .execute(
                &serde_json::json!({"path": "notes.md", "limit": 1}),
                &cx(&dir),
            )
            .await;
        assert!(!read.is_error(), "the ranged read itself is fine");

        let result = WriteFile::with_ledger(ledger)
            .execute(
                &serde_json::json!({"path": "notes.md", "content": "clobbered"}),
                &cx(&dir),
            )
            .await;
        assert!(result.is_error(), "a 1-of-3-line read is not coverage");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the witness: having actually seen the file, the model
    /// may replace it. A guard that refused here would be a wall, not a gate.
    #[tokio::test]
    async fn an_overwrite_after_a_whole_file_read_is_allowed() {
        let dir = scratch("whole");
        std::fs::write(dir.join("notes.md"), "one\ntwo\nthree\n").expect("seed");
        let ledger = Arc::new(ReadLedger::default());

        let read = crate::read::ReadFile::with_ledger(ledger.clone())
            .execute(&serde_json::json!({"path": "notes.md"}), &cx(&dir))
            .await;
        assert!(
            !read.is_error(),
            "the read must succeed for the test to mean anything"
        );

        let result = WriteFile::with_ledger(ledger)
            .execute(
                &serde_json::json!({"path": "notes.md", "content": "rewritten"}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("wrote 9 bytes")),
            ToolOutput::Error { message, .. } => panic!("expected the write to land: {message}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).expect("rewritten"),
            "rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A successful write earns its own coverage: the model authored every
    /// byte on disk, so the guard must not then refuse it access to what it
    /// just produced.
    #[tokio::test]
    async fn a_write_covers_the_file_it_just_wrote() {
        let dir = scratch("rewrite");
        let ledger = Arc::new(ReadLedger::default());
        let tool = WriteFile::with_ledger(ledger);

        let first = tool
            .execute(
                &serde_json::json!({"path": "fresh.txt", "content": "v1"}),
                &cx(&dir),
            )
            .await;
        assert!(!first.is_error(), "creating a file needs no read");

        let second = tool
            .execute(
                &serde_json::json!({"path": "fresh.txt", "content": "v2"}),
                &cx(&dir),
            )
            .await;
        match second {
            ToolOutput::Ok { .. } => {}
            ToolOutput::Error { message, .. } => {
                panic!("a rewrite of content the model authored must pass: {message}")
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn missing_content_returns_error() {
        let dir = std::env::temp_dir();
        let result = WriteFile::default()
            .execute(&serde_json::json!({"path": "ok.txt"}), &cx(&dir))
            .await;
        assert!(result.is_error());
    }

    // ── batching (#4151) ──────────────────────────────────────────────────

    #[tokio::test]
    async fn one_call_writes_several_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = WriteFile::default()
            .execute(
                &serde_json::json!({"files": [
                    {"path": "a.rs", "content": "fn a() {}\n"},
                    {"path": "nested/b.rs", "content": "fn b() {}\n"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() {}\n"
        );
        // Parent directories are still created, as the single form does.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/b.rs")).unwrap(),
            "fn b() {}\n"
        );
    }

    /// Every guard is asked before any byte is written, so a batch refused on
    /// its second file does not leave the first one on disk. A write cannot be
    /// rolled back once the old bytes are gone; checking first is the same
    /// guarantee for the case that actually happens.
    #[tokio::test]
    async fn a_batch_refused_on_a_later_file_writes_none_of_them() {
        let dir = tempfile::tempdir().unwrap();
        // An existing file this session has never read: the no-clobber guard
        // must refuse it, and take the whole batch down with it.
        std::fs::write(dir.path().join("existing.rs"), "original\n").unwrap();

        let out = WriteFile::default()
            .execute(
                &serde_json::json!({"files": [
                    {"path": "fresh.rs", "content": "new\n"},
                    {"path": "existing.rs", "content": "clobbered\n"}
                ]}),
                &cx(dir.path()),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("the no-clobber guard must refuse the batch: {out:?}");
        };
        assert!(message.contains("Nothing was written"), "{message}");
        assert!(
            !dir.path().join("fresh.rs").exists(),
            "the file that would have succeeded must not have landed"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("existing.rs")).unwrap(),
            "original\n"
        );
    }
}
