//! Capability-minimal tool surface for candidate-local witness authoring.
//!
//! The witness author's tools are **stage-private**: a file reader and a
//! glob-pattern file lister — both confined to the candidate snapshot — plus
//! the one mutation, `create_witness_test`. None of them is a registry
//! built-in: they exist only inside the candidate-workspace mount, are never
//! model-visible in an ordinary session, and their lifecycle is the witness
//! stage's, not the registry's.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use stella_core::ToolExecutor;
use stella_pipeline::ports::WorkspaceError;
use stella_protocol::{ToolOutput, ToolSchema};

/// Ceiling on one `read_file` answer. A witness author reads source and test
/// files to decide what to pin; a file past this bound is truncated with a
/// note rather than flooding the authoring turn.
const READ_CAP_BYTES: usize = 131_072;

/// Ceiling on one `glob` answer, in matched paths. Past it the tail is
/// summarized as a count so a `**/*` over a large tree stays a listing, not
/// a flood.
const GLOB_MATCH_CAP: usize = 2_000;

/// Candidate-root executor built specifically for witness authoring. Reads
/// stay in the snapshot; the sole mutation atomically creates one new test,
/// and a repeat create replaces the previous artifact so at most one witness
/// file ever exists (the repair turn's "rewrite the test" depends on this).
pub(super) struct WitnessToolExecutor {
    root: PathBuf,
    /// Absolute roots the authored source must not anchor itself to (#2130):
    /// the session's own workspace root — the frame the goal's paths are
    /// written in — and this snapshot's root, which the graft leaves behind.
    /// Ordered session-first, because that is the spelling an author copies
    /// out of the goal and so the one worth naming in the refusal.
    tree_roots: Vec<String>,
    /// The operator's tool switches (#1784): a `"tools": {"read_file":
    /// "off"}` (or `"glob": "off"`) entry withholds the author's matching
    /// read exactly as a switch withholds any session tool — the schema is
    /// not offered and dispatch refuses by name.
    policy: stella_tools::policy::ToolPolicy,
    created_path: Mutex<Option<String>>,
}

impl WitnessToolExecutor {
    pub(super) fn new(
        root: PathBuf,
        session_root: &Path,
        policy: stella_tools::policy::ToolPolicy,
    ) -> Self {
        let mut tree_roots = vec![session_root.display().to_string()];
        let snapshot = root.display().to_string();
        if !tree_roots.contains(&snapshot) {
            tree_roots.push(snapshot);
        }
        Self {
            root,
            tree_roots,
            policy,
            created_path: Mutex::new(None),
        }
    }

    fn denied(name: &str, reason: impl std::fmt::Display) -> ToolOutput {
        ToolOutput::error(format!(
            "`{name}` is not available to the witness author: {reason}"
        ))
    }

    /// The stage-private file reader: one workspace-relative path, answered
    /// with the file's content (truncated past [`READ_CAP_BYTES`]).
    fn read_file(&self, input: &serde_json::Value) -> ToolOutput {
        let name = "read_file";
        if !self.policy.allows(name) {
            return Self::denied(name, "it is switched off by the operator's tool policy");
        }
        let Some(raw_path) = input.get("path").and_then(serde_json::Value::as_str) else {
            return Self::denied(name, "a workspace-relative `path` is required");
        };
        let Some(path) = normalized_candidate_path(raw_path) else {
            return Self::denied(name, "the path must stay within the candidate root");
        };
        if is_credential_path(&path) {
            return Self::denied(name, "credential and private-state paths are excluded");
        }
        let Some(full) = confined(&self.root, &path) else {
            return Self::denied(
                name,
                format!("`{path}` does not resolve inside the candidate root"),
            );
        };
        let metadata = match std::fs::metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) => return Self::denied(name, format!("`{path}`: {error}")),
        };
        if !metadata.is_file() {
            return Self::denied(name, format!("`{path}` is not a regular file"));
        }
        let bytes = match std::fs::read(&full) {
            Ok(bytes) => bytes,
            Err(error) => return Self::denied(name, format!("`{path}`: {error}")),
        };
        let truncated = bytes.len() > READ_CAP_BYTES;
        let shown = if truncated {
            // Never split a UTF-8 sequence: back off to a char boundary of
            // the lossy rendering below by trimming raw bytes first and
            // letting `from_utf8_lossy` absorb a clipped tail sequence.
            &bytes[..READ_CAP_BYTES]
        } else {
            &bytes[..]
        };
        let mut content = String::from_utf8_lossy(shown).into_owned();
        if truncated {
            content.push_str("\n… [truncated: the file continues past the read ceiling]");
        }
        ToolOutput::Ok {
            content,
            data: None,
        }
    }

    /// The stage-private lister: candidate files whose paths match a glob
    /// pattern, names only, root-confined, credential paths excluded.
    fn glob(&self, input: &serde_json::Value) -> ToolOutput {
        let name = "glob";
        if !self.policy.allows(name) {
            return Self::denied(name, "it is switched off by the operator's tool policy");
        }
        let Some(pattern) = input.get("pattern").and_then(serde_json::Value::as_str) else {
            return Self::denied(name, "a `pattern` is required");
        };
        if pattern.trim().is_empty() {
            return Self::denied(name, "the `pattern` must not be empty");
        }
        // The root itself may be spelled `.`, `""` or `./` — the default and
        // the blind author's whole opening move, so it must pass. A subdir
        // narrows the search; anything else is an escape.
        let search_rel = match input.get("path").and_then(serde_json::Value::as_str) {
            None => None,
            Some(raw) if names_candidate_root(raw) => None,
            Some(raw) => match normalized_candidate_path(raw) {
                Some(rel) => Some(rel),
                None => {
                    return Self::denied(name, "the path must stay within the candidate root");
                }
            },
        };
        let Ok(root) = self.root.canonicalize() else {
            return Self::denied(name, "the candidate root is unavailable");
        };
        let search_dir = match &search_rel {
            None => root.clone(),
            Some(rel) => {
                let Some(dir) = confined(&root, rel) else {
                    return Self::denied(
                        name,
                        format!("`{rel}` does not resolve inside the candidate root"),
                    );
                };
                if !dir.is_dir() {
                    return Self::denied(name, format!("`{rel}` is not a directory"));
                }
                dir
            }
        };

        let mut matches = Vec::new();
        let mut overflow = 0usize;
        walk_matching(&root, &search_dir, pattern, &mut matches, &mut overflow);
        matches.sort();
        if matches.is_empty() {
            return ToolOutput::Ok {
                content: format!("no files match `{pattern}`"),
                data: None,
            };
        }
        let mut content = matches.join("\n");
        if overflow > 0 {
            content.push_str(&format!("\n… and {overflow} more (narrow the pattern)"));
        }
        ToolOutput::Ok {
            content,
            data: None,
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
        // The assertion-density screen (#863). This runs BEFORE the one-create
        // claim is taken, so a refusal leaves the author's single write still
        // available: the named reason arrives as a tool error and it revises
        // in-turn, rather than the run discovering after a baseline test run
        // and a repair turn that the artifact could never have witnessed
        // anything. This boundary is the only path witness bytes take to disk,
        // which is what makes a check here complete rather than advisory.
        if let Err(vacuous) =
            stella_pipeline::witness::density::screen_witness_source(&path, content)
        {
            return Self::denied("create_witness_test", vacuous);
        }
        // The path-frame screen (#2130), here for exactly the reasons the
        // density screen is: the author is told the frame in its prompt, and
        // prose is guidance — this is the enforcement, at the one boundary
        // witness bytes take to disk. A witness that hardcodes the project's
        // absolute paths is unsatisfiable by construction (the oracle runs it
        // inside a copy of the tree rooted elsewhere), and refusing it here
        // costs the author nothing: the one-create claim below is untaken, so
        // it revises in-turn against a message that names the relative form.
        if let Err(misframed) =
            stella_pipeline::witness::frame::screen_witness_frame(&self.tree_roots, content)
        {
            return Self::denied("create_witness_test", misframed);
        }

        let mut claimed = self
            .created_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Ok(root) = self.root.canonicalize() else {
            return Self::denied("create_witness_test", "the candidate root is unavailable");
        };
        // Replace-your-own-artifact, never accumulate: at most one witness
        // file exists at any moment, and a second create deletes the first
        // before writing. This is what makes the bounded repair turn able to
        // act at all — its instruction is "rewrite the test", and a claim
        // that never resets turned every repair into a paid model call whose
        // only legal move was narrowing the command against an unchanged
        // file. The single-artifact invariant the claim exists for is
        // preserved: acceptance still requires exactly one new file.
        if let Some(previous) = claimed.take() {
            let prior = root.join(&previous);
            if let Err(error) = std::fs::remove_file(&prior) {
                *claimed = Some(previous);
                return Self::denied(
                    "create_witness_test",
                    format!("replacing the previous witness artifact failed: {error}"),
                );
            }
        }
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
            data: None,
        }
    }
}

#[async_trait]
impl ToolExecutor for WitnessToolExecutor {
    // `live_services` keeps the empty default on purpose (#2764). This is a
    // witness author's surface — `read_file`, `glob`, `create_witness_test`
    // and nothing else — so it can neither start a service nor stop one, and
    // reporting the SESSION's services here would interrupt one agent's turn
    // about another agent's workspace state. The assertion belongs to the
    // turn that owns the process, which is the worker's.
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = Vec::new();
        if self.policy.allows("glob") {
            schemas.push(ToolSchema {
                name: "glob".into(),
                description: "List candidate files whose workspace-relative paths match a glob pattern (`*`, `?`, `**`). Names only; `path` narrows the search to a subdirectory.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern matched against workspace-relative paths" },
                        "path": { "type": "string", "description": "Subdirectory to search (default: the candidate root)" }
                    },
                    "required": ["pattern"]
                }),
                read_only: true,
                speculation_safe: true,
            });
        }
        if self.policy.allows("read_file") {
            schemas.push(ToolSchema {
                name: "read_file".into(),
                description: "Read one file inside the candidate, by workspace-relative path."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Workspace-relative file path" }
                    },
                    "required": ["path"]
                }),
                read_only: true,
                speculation_safe: true,
            });
        }
        schemas.push(ToolSchema {
            name: "create_witness_test".into(),
            description: "Atomically create one previously absent test file inside the candidate. Existing files are refused; calling again replaces your previous artifact.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "New test file path relative to the candidate root" },
                    "content": { "type": "string", "description": "Complete test source" }
                },
                "required": ["path", "content"]
            }),
            read_only: false,
            speculation_safe: false,
        });
        schemas
    }

    async fn execute(&self, name: &str, input: &serde_json::Value) -> ToolOutput {
        match name {
            "read_file" => self.read_file(input),
            "glob" => self.glob(input),
            "create_witness_test" => self.create_test(input),
            _ => Self::denied(
                name,
                "only candidate reads and atomic witness creation are allowed",
            ),
        }
    }
}

/// Resolve the workspace-relative `rel` (already vetted by
/// [`normalized_candidate_path`]) to its canonical on-disk location and prove
/// it stays inside `root`: both sides are canonicalized — symlinks resolved —
/// and the resolved location must sit under the resolved root. Layered on
/// top of the lexical screen so a symlink planted *inside* the snapshot can
/// never reach outside it: the lexical screen refuses `..`/absolute
/// spellings by name, and this check refuses what the filesystem actually
/// resolves them to. `None` when the path does not resolve (absent,
/// unreadable) or escapes.
fn confined(root: &Path, rel: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let full = root.join(rel).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

/// Recursively collect files under `dir` whose paths relative to the
/// candidate `root` match `pattern`, excluding credential paths from both
/// the answer and — where nothing readable can live below — the descent.
///
/// Matching is against the path relative to the search directory, so a
/// `tests/*.rs` pattern means the same thing whether the search starts at
/// the root or at `path: "tests"`'s parent; emitted paths are root-relative
/// so the author can hand them straight to `read_file`.
fn walk_matching(
    root: &Path,
    dir: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
    overflow: &mut usize,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let Ok(rel_to_root) = entry_path.strip_prefix(root) else {
            continue;
        };
        let rel = rel_to_root.to_string_lossy().replace('\\', "/");
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Symlinks are not followed: a link's target may sit outside the
        // snapshot, and names-only discovery loses nothing by skipping it.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            // Prune descent where nothing readable can live below. `.git`
            // itself still descends — what follows it is judged entry by
            // entry (refs and logs are evidence; objects and hooks are not).
            let is_git_dir = rel == ".git" || rel.ends_with("/.git");
            if is_credential_path(&rel) && !is_git_dir {
                continue;
            }
            walk_matching(root, &entry_path, pattern, matches, overflow);
        } else if file_type.is_file() && !is_credential_path(&rel) && glob_match(pattern, &rel) {
            if matches.len() < GLOB_MATCH_CAP {
                matches.push(rel);
            } else {
                *overflow += 1;
            }
        }
    }
}

/// Match a glob `pattern` against a `/`-separated relative path.
///
/// Supported syntax: `*` (any run of characters within one component), `?`
/// (exactly one character within a component), and `**` as a whole component
/// (zero or more components). Everything else matches literally; there are
/// no character classes or brace sets — the author's discovery needs names,
/// not a pattern language.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let path: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match_components(&pattern, &path)
}

fn match_components(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_components(rest, &path[skip..])),
        Some((first, rest)) => path.split_first().is_some_and(|(segment, tail)| {
            component_match(first, segment) && match_components(rest, tail)
        }),
    }
}

/// `*`/`?` wildcard match within one path component.
fn component_match(pattern: &str, segment: &str) -> bool {
    fn matches(pattern: &[char], segment: &[char]) -> bool {
        match pattern.split_first() {
            None => segment.is_empty(),
            // Consecutive `*`s collapse to one, keeping the backtracking
            // linear in the segment for the patterns authors actually write.
            Some(('*', rest)) if rest.first() == Some(&'*') => matches(rest, segment),
            Some(('*', rest)) => (0..=segment.len()).any(|skip| matches(rest, &segment[skip..])),
            Some(('?', rest)) => segment
                .split_first()
                .is_some_and(|(_, tail)| matches(rest, tail)),
            Some((expected, rest)) => segment
                .split_first()
                .is_some_and(|(actual, tail)| actual == expected && matches(rest, tail)),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let segment: Vec<char> = segment.chars().collect();
    matches(&pattern, &segment)
}

/// Does this `path` name the candidate root itself?
///
/// Separate from [`normalized_candidate_path`] rather than folded into it: that
/// function normalizes to a non-empty *relative* path, and the root does not
/// have one, so `None` there means both "escapes the root" and "is the root".
/// Only the listing tools can act on the second, and conflating them is what
/// made `glob` refuse its own default argument.
///
/// Absolute and drive-qualified spellings are excluded first, so `/` — which
/// would otherwise trim to the empty string — stays an escape.
fn names_candidate_root(raw: &str) -> bool {
    let slash = raw.replace('\\', "/");
    if Path::new(&slash).is_absolute() || slash.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    matches!(slash.trim_end_matches('/'), "" | ".")
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

/// The `.git` entries a witness author may read, given the path components
/// that follow the `.git` component itself.
///
/// A blanket `.git` denial is right for `.git/config` (a remote URL can
/// carry a token), right for `.git/hooks` (executable code), and useless for
/// `.git/HEAD`. On a git-shaped goal it is worse than useless: it withholds
/// the *entire* evidence surface from the one role whose job is to assert
/// something true about the repository. A `fix-git` bench trial recorded the
/// result — the author's `read_file` calls on `.git/HEAD` and
/// `.git/logs/HEAD` were both refused as "credential and private-state
/// paths", so it reasoned counterfactually about a repository it was
/// forbidden to look at, and insured against its own uncertainty by writing
/// a universally quantified assertion far stronger than the goal. The
/// witness then failed on work that was correct.
///
/// An allowlist, not a narrowed denylist: a `.git` entry added by a future
/// git version is denied until someone decides it is safe, which is the
/// direction this predicate should fail in.
fn is_readable_git_metadata(rest: &[String]) -> bool {
    match rest.first().map(String::as_str) {
        // Ref storage and the position log — the "where has this been?"
        // evidence. `worktrees/` carries a linked worktree's own HEAD/refs.
        Some("refs" | "logs" | "worktrees") => true,
        Some(name) => matches!(
            name,
            "head" | "orig_head" | "merge_head" | "fetch_head" | "cherry_pick_head" | "packed-refs"
        ),
        // `.git` itself: a directory, never a file to read.
        None => false,
    }
}

fn is_credential_path(path: &str) -> bool {
    let components: Vec<_> = path
        .split('/')
        .map(|component| component.to_ascii_lowercase())
        .collect();
    if components
        .windows(2)
        .any(|pair| pair == [".stella", "private"])
    {
        return true;
    }
    // `.git` is judged POSITIONALLY — what follows it decides — so it is
    // resolved before the component-wise denylist rather than inside it.
    if let Some(idx) = components.iter().position(|component| component == ".git") {
        return !is_readable_git_metadata(&components[idx + 1..]);
    }
    components.iter().any(|component| {
        matches!(
            component.as_str(),
            ".ssh" | ".aws" | ".azure" | ".config" | ".kube"
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

    /// Witness: a git-shaped goal needs git evidence, and `.git` sat on the
    /// blanket credential denylist. The author's `read_file` on `.git/HEAD`
    /// and `.git/logs/HEAD` were both refused in a `fix-git` trial, so it
    /// wrote its assertion from a guess about a repository it could not see.
    #[test]
    fn git_metadata_is_readable_while_git_secrets_stay_denied() {
        for readable in [
            ".git/HEAD",
            ".git/ORIG_HEAD",
            ".git/packed-refs",
            ".git/refs/heads/main",
            ".git/logs/HEAD",
            ".git/worktrees/candidate/HEAD",
            "nested/.git/refs/heads/main",
        ] {
            assert!(
                !is_credential_path(readable),
                "{readable} is evidence, not a credential"
            );
        }

        for denied in [
            // A remote URL in `config` can carry a token.
            ".git/config",
            // Executable code, and the one `.git` path that can act.
            ".git/hooks/pre-commit",
            ".git/credentials",
            // Not an allowlisted entry: unknown `.git` entries stay denied.
            ".git/objects/ab/cdef",
            ".git",
        ] {
            assert!(is_credential_path(denied), "{denied} must stay denied");
        }
    }

    /// The rest of the denylist is untouched by making `.git` positional.
    #[test]
    fn the_non_git_credential_denylist_is_unchanged() {
        for denied in [
            ".ssh/id_rsa",
            ".aws/credentials",
            ".kube/config",
            ".env",
            ".env.production",
            "certs/server.pem",
            "certs/server.key",
            ".stella/private/store.db",
            "deploy/credentials.json",
        ] {
            assert!(is_credential_path(denied), "{denied} must stay denied");
        }
        assert!(!is_credential_path("src/main.rs"));
        assert!(!is_credential_path("tests/env_test.rs"));
    }

    /// The matcher's contract, spelled out: `*`/`?` stay inside a component,
    /// `**` crosses them, and everything else is literal.
    #[test]
    fn glob_matching_covers_the_supported_syntax() {
        for (pattern, path) in [
            ("**/*", "src.rs"),
            ("**/*", "tests/existing.rs"),
            ("**/*.rs", "a/b/c/d.rs"),
            ("tests/*.rs", "tests/existing.rs"),
            ("tests/**/*.rs", "tests/existing.rs"),
            ("tests/e?isting.rs", "tests/existing.rs"),
            ("**/existing.rs", "tests/existing.rs"),
        ] {
            assert!(glob_match(pattern, path), "`{pattern}` must match `{path}`");
        }
        for (pattern, path) in [
            ("*.rs", "tests/existing.rs"),
            ("tests/*.py", "tests/existing.rs"),
            ("tests/?.rs", "tests/existing.rs"),
            ("src/**", "tests/existing.rs"),
        ] {
            assert!(
                !glob_match(pattern, path),
                "`{pattern}` must not match `{path}`"
            );
        }
    }

    fn witness_executor(root: &Path) -> WitnessToolExecutor {
        witness_executor_for(root, root)
    }

    /// The two-tree shape the production path always has (#2130): the author
    /// stands in a snapshot while the goal's paths are phrased against the
    /// session's own root. `witness_executor` collapses them because the tests
    /// that use it are about file mechanics, where the distinction is noise.
    fn witness_executor_for(root: &Path, session_root: &Path) -> WitnessToolExecutor {
        WitnessToolExecutor::new(
            root.to_path_buf(),
            session_root,
            stella_tools::policy::ToolPolicy::allow_all(),
        )
    }

    /// A witness body that would really witness something, tagged so the tests
    /// below can still tell two attempts apart.
    ///
    /// The tests that use this are about file *mechanics* — exclusive creation,
    /// symlink refusal, the one-create claim under a race — and they used to
    /// pass placeholder text like `"new"`. The density screen (#863) refuses
    /// that before any of those mechanics run, which silently turned the
    /// symlink test green for the wrong reason. Real bytes keep each test
    /// testing what it names.
    fn witness_body(tag: u32) -> String {
        format!("#[test]\nfn witness() {{\n    assert_eq!(stella::compute(), {tag});\n}}\n")
    }

    #[tokio::test]
    async fn has_no_general_or_external_capabilities() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("src.rs"), "source").unwrap();
        std::fs::write(root.path().join(".env.local"), "secret").unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/config"), "credential-helper").unwrap();
        let tools = witness_executor(root.path());

        let names: Vec<_> = tools
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect();
        assert_eq!(names, vec!["glob", "read_file", "create_witness_test"]);
        for denied in [
            "save_state",
            "get_state",
            "task",
            "task_create",
            "get_environment",
            "mcp_external",
            "custom_script",
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

    /// The reader is root-confined by resolution, not by spelling alone: a
    /// symlink inside the snapshot pointing outside it does not resolve
    /// inside the root, so the read is refused even though its spelling
    /// passes the lexical screen.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_file_refuses_a_symlink_escaping_the_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            root.path().join("escape.txt"),
        )
        .unwrap();
        let tools = witness_executor(root.path());

        assert!(
            tools
                .execute("read_file", &serde_json::json!({"path": "escape.txt"}))
                .await
                .is_error(),
            "a symlink out of the snapshot must not be readable"
        );
    }

    #[tokio::test]
    async fn exclusive_creation_never_mutates_an_existing_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let existing = root.path().join("tests/existing.rs");
        std::fs::write(&existing, "original").unwrap();
        let tools = witness_executor(root.path());

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/existing.rs", "content": witness_body(1)}),
            )
            .await;
        assert!(output.is_error());
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "original");

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/new_witness.rs", "content": witness_body(2)}),
            )
            .await;
        assert!(matches!(output, ToolOutput::Ok { .. }));
        assert_eq!(
            std::fs::read_to_string(root.path().join("tests/new_witness.rs")).unwrap(),
            witness_body(2)
        );
        // A further create is the repair turn's rewrite: it REPLACES the
        // previous artifact rather than being refused, and at most one
        // witness file exists afterwards. Before this contract, the repair
        // turn's "rewrite the test" instruction was structurally impossible —
        // the claim never reset, so every repair-turn create was denied.
        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/second.rs", "content": witness_body(3)}),
            )
            .await;
        assert!(
            matches!(output, ToolOutput::Ok { .. }),
            "a repeat create must replace the author's own artifact: {output:?}"
        );
        assert!(
            !root.path().join("tests/new_witness.rs").exists(),
            "the replaced artifact must be discarded"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("tests/second.rs")).unwrap(),
            witness_body(3)
        );
    }

    /// #863 at the boundary that owns it: a vacuous witness never reaches
    /// disk, the reason is named, and the author's one create is still
    /// available — a refusal that consumed the write would turn a revisable
    /// mistake into a lost witness.
    #[tokio::test]
    async fn a_vacuous_witness_is_refused_and_the_author_may_still_revise() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let tools = witness_executor(root.path());

        for (name, content, expected) in [
            (
                "tests/no_assertions.rs",
                "#[test]\nfn witness() {\n    let _ = stella::retry();\n}\n",
                "asserts nothing",
            ),
            (
                "tests/tautology.rs",
                "#[test]\nfn witness() {\n    assert_eq!(2, 2);\n}\n",
                "tautology over constants",
            ),
            (
                "tests/catch_all.rs",
                "#[test]\n#[should_panic]\nfn witness() {\n    stella::retry();\n}\n",
                "catch-all panic check",
            ),
        ] {
            let output = tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": name, "content": content}),
                )
                .await;
            match &output {
                ToolOutput::Error { message, .. } => assert!(
                    message.contains(expected),
                    "{name} must name its vacuous shape, got: {message}"
                ),
                ToolOutput::Ok { .. } => panic!("{name} is vacuous and must be refused"),
            }
            assert!(
                !root.path().join(name).exists(),
                "{name} must never reach disk"
            );
        }

        // The refusals did not consume the one-create budget: a real witness
        // still lands on the very next attempt, in the same turn.
        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({
                    "path": "tests/real_witness.rs",
                    "content": "#[test]\nfn witness() {\n    \
                                assert_eq!(stella::retry_delays(3), vec![1, 2, 4]);\n}\n",
                }),
            )
            .await;
        assert!(
            matches!(output, ToolOutput::Ok { .. }),
            "a substantive witness must be accepted after the refusals: {output:?}"
        );
        assert!(root.path().join("tests/real_witness.rs").exists());
    }

    /// #2130's witness: a witness anchored to a tree it will not run in is
    /// refused at the boundary, and the author still has its create.
    ///
    /// This is the `openssl-selfsigned-cert` shape (match `cc00894779ff`): the
    /// goal named `/app/ssl/server.crt`, so the author wrote that down, and the
    /// oracle ran the script inside the candidate copy — where it failed
    /// identically for every change, correct or not, and rode a finished trial
    /// into its timeout at reward 0. Both roots are refused: anchoring to the
    /// snapshot the author is standing in is just as unsatisfiable, because
    /// the graft moves the test into a different candidate before the oracle
    /// sees it.
    #[tokio::test]
    async fn a_witness_anchored_to_a_tree_it_will_not_run_in_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let tools = witness_executor_for(root.path(), Path::new("/app"));

        let misframed = format!(
            "#!/bin/sh\nset -e\nSSL_DIR=/app/ssl\ntest -f \"$SSL_DIR/server.crt\"\n\
             test -f {}/ssl/server.key\n",
            root.path().display()
        );
        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "witness_ssl_cert.sh", "content": misframed}),
            )
            .await;
        match &output {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("/app/ssl"),
                    "the refusal must quote what was written: {message}"
                );
                assert!(
                    message.contains("`ssl`"),
                    "and hand back the relative form: {message}"
                );
            }
            ToolOutput::Ok { .. } => {
                panic!("a witness pinned to the project root can never pass its oracle")
            }
        }
        assert!(
            !root.path().join("witness_ssl_cert.sh").exists(),
            "the misframed artifact must never reach disk"
        );

        // The snapshot's own root is refused too — the artifact is grafted
        // into a different candidate before the flip is observed.
        let snapshot_pinned = format!(
            "#!/bin/sh\ntest -f {}/ssl/server.crt\n",
            root.path().display()
        );
        assert!(
            tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "witness_ssl_cert.sh", "content": snapshot_pinned}),
                )
                .await
                .is_error(),
            "the authoring snapshot is not the tree the oracle runs in either"
        );

        // Neither refusal consumed the one create: the correctly framed
        // rewrite lands on the very next attempt, in the same turn.
        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({
                    "path": "witness_ssl_cert.sh",
                    "content": "#!/bin/sh\nset -e\ntest -f ssl/server.crt\ntest -f ssl/server.key\n",
                }),
            )
            .await;
        assert!(
            matches!(output, ToolOutput::Ok { .. }),
            "the relative rewrite must be accepted after the refusals: {output:?}"
        );
        assert!(root.path().join("witness_ssl_cert.sh").exists());
    }

    /// The screen refuses the tree under test and nothing else: a witness
    /// legitimately names its interpreter and the tools it shells out to, and
    /// an infra deliverable outside the workspace root is not the copied tree.
    #[tokio::test]
    async fn machine_paths_stay_available_to_the_witness_author() {
        let root = tempfile::tempdir().unwrap();
        let tools = witness_executor_for(root.path(), Path::new("/app"));

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({
                    "path": "witness_nginx.sh",
                    "content": "#!/bin/sh\nset -e\ntest -x /usr/sbin/nginx\n\
                                grep -q 'ssl_protocols TLSv1.3' /etc/nginx/nginx.conf\n",
                }),
            )
            .await;
        assert!(
            matches!(output, ToolOutput::Ok { .. }),
            "machine and out-of-tree paths must stay usable: {output:?}"
        );
    }

    /// #1792's witness: the blind author can discover the tree by name —
    /// `glob` is offered and executes root-confined, and its results pass
    /// the same credential exclusion as the read path, so discovery never
    /// becomes a map of the workspace's secrets.
    #[tokio::test]
    async fn glob_lets_the_author_discover_tests_without_leaking_credentials() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("tests/existing.rs"), "#[test] fn t() {}").unwrap();
        std::fs::write(root.path().join(".env.local"), "secret").unwrap();
        let tools = witness_executor(root.path());

        let output = tools
            .execute("glob", &serde_json::json!({"pattern": "**/*"}))
            .await;
        let ToolOutput::Ok { content, .. } = output else {
            panic!("glob must be available to the witness author: {output:?}");
        };
        assert!(
            content.contains("tests/existing.rs"),
            "discovery must surface the test directory: {content}"
        );
        assert!(
            !content.contains(".env.local"),
            "credential paths must not appear in discovery output: {content}"
        );

        let escaped = tools
            .execute(
                "glob",
                &serde_json::json!({"pattern": "*", "path": "../.."}),
            )
            .await;
        assert!(
            escaped.is_error(),
            "an escaping search path must be refused"
        );
    }

    /// Naming the root explicitly is the same call as omitting `path`.
    ///
    /// `path` is documented as "Subdirectory to search (default: the
    /// candidate root)" and `.` and `""` resolve to the root, so a model that
    /// spells the default out loud — which they routinely do — must not be
    /// refused. [`normalized_candidate_path`]'s `None` means "no relative
    /// path" and therefore covers both "escapes the root" and "*is* the
    /// root"; only the second is legal, and collapsing them shuts the door
    /// on the blind author's opening move (#1792).
    #[tokio::test]
    async fn glob_accepts_the_root_spelled_out_as_well_as_omitted() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("tests/existing.rs"), "#[test] fn t() {}").unwrap();
        let tools = witness_executor(root.path());

        for path in [".", "", "./"] {
            let output = tools
                .execute(
                    "glob",
                    &serde_json::json!({"pattern": "**/*", "path": path}),
                )
                .await;
            let ToolOutput::Ok { content, .. } = output else {
                panic!("`path: {path:?}` names the root and must be allowed: {output:?}");
            };
            assert!(
                content.contains("tests/existing.rs"),
                "`path: {path:?}` must search the root: {content}"
            );
        }

        // The root spellings are the only ones that gain entry: `/` trims to
        // the empty string but is absolute, and must stay an escape.
        for path in ["/", "..", "../..", "/etc"] {
            assert!(
                tools
                    .execute("glob", &serde_json::json!({"pattern": "*", "path": path}))
                    .await
                    .is_error(),
                "`path: {path:?}` leaves the candidate root and must be refused"
            );
        }
    }

    /// #1784's witness: the operator's tool policy governs the witness
    /// author's reads exactly as it governs the worker's. The executor
    /// carries the policy the candidate workspace hands it, so a
    /// `read_file: off` switch removes both the schema and the dispatch
    /// here, not just on the worker path.
    #[tokio::test]
    async fn the_tool_policy_reaches_the_witness_authors_reads() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("src.rs"), "source").unwrap();
        let tools = WitnessToolExecutor::new(
            root.path().to_path_buf(),
            root.path(),
            stella_tools::policy::ToolPolicy::from_switches([("read_file".to_string(), false)]),
        );

        let names: Vec<_> = tools
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect();
        assert_eq!(
            names,
            vec!["glob", "create_witness_test"],
            "a switched-off read_file must not be offered to the author (glob stays)"
        );
        assert!(
            tools
                .execute("read_file", &serde_json::json!({"path": "src.rs"}))
                .await
                .is_error(),
            "and must not execute either"
        );
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
        let tools = witness_executor(root.path());

        let output = tools
            .execute(
                "create_witness_test",
                &serde_json::json!({"path": "tests/witness.rs", "content": witness_body(1)}),
            )
            .await;
        assert!(output.is_error());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside");
    }

    #[tokio::test]
    async fn concurrent_creates_commit_at_most_one_artifact() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let left_tools = std::sync::Arc::new(witness_executor(root.path()));
        let right_tools = std::sync::Arc::new(witness_executor(root.path()));
        let left = tokio::spawn(async move {
            left_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/raced.rs", "content": witness_body(1)}),
                )
                .await
        });
        let right = tokio::spawn(async move {
            right_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/raced.rs", "content": witness_body(2)}),
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
        assert!(
            content == witness_body(1) || content == witness_body(2),
            "{content}"
        );
    }

    /// The claim's invariant under concurrency is *at most one artifact on
    /// disk*, not *at most one successful call*: a later create replaces the
    /// earlier artifact (the repair turn's rewrite), and the mutex serializes
    /// the replacement so no interleaving can leave two files behind.
    #[tokio::test]
    async fn one_executor_holds_at_most_one_artifact_under_concurrency() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tests")).unwrap();
        let tools = std::sync::Arc::new(witness_executor(root.path()));
        let left_tools = tools.clone();
        let left = tokio::spawn(async move {
            left_tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/left.rs", "content": witness_body(1)}),
                )
                .await
        });
        let right = tokio::spawn(async move {
            tools
                .execute(
                    "create_witness_test",
                    &serde_json::json!({"path": "tests/right.rs", "content": witness_body(2)}),
                )
                .await
        });

        let (left, right) = tokio::join!(left, right);
        for output in [left.unwrap(), right.unwrap()] {
            assert!(
                !output.is_error(),
                "serialized creates each succeed; the later one replaces: {output:?}"
            );
        }
        let created = ["tests/left.rs", "tests/right.rs"]
            .iter()
            .filter(|path| root.path().join(path).exists())
            .count();
        assert_eq!(created, 1, "the replaced artifact must not survive");
    }

    /// #3148: the witness surface is the one tool mount outside
    /// `stella_tools::catalog` — a **declared** exemption, pinned from both
    /// sides.
    ///
    /// These are deliberately not catalog rows: they exist only inside the
    /// candidate-workspace mount, must never be model-visible in an ordinary
    /// session, and the artifact's lifecycle is the witness stage's, not the
    /// registry's. The catalog pins (`registry_advertises_exactly_the_catalog
    /// _tool_set`) cannot see this surface because it never enters a
    /// `ToolRegistry` — so nothing would notice a bespoke tool accumulating
    /// out here either. Now:
    ///
    /// - a new out-of-catalog name on this surface fails here until it is
    ///   cataloged or added to `EXEMPT` with its own issue;
    /// - an exempt name joining the catalog fails here too, so the exemption
    ///   is retired instead of going stale.
    #[test]
    fn the_out_of_catalog_surface_is_exactly_the_declared_exemption() {
        const EXEMPT: &[&str] = &["glob", "read_file", "create_witness_test"];

        let root = tempfile::tempdir().unwrap();
        let tools = witness_executor(root.path());

        let advertised: Vec<String> = tools
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect();
        let out_of_catalog: Vec<&str> = advertised
            .iter()
            .map(String::as_str)
            .filter(|name| !stella_tools::catalog::ALL_NAMES.contains(name))
            .collect();
        assert_eq!(
            out_of_catalog, EXEMPT,
            "every tool this executor advertises beyond the declared \
             exemption must be a catalog row (#3148)"
        );
        for name in EXEMPT {
            assert!(
                !stella_tools::catalog::ALL_NAMES.contains(name),
                "{name} joined the catalog — retire its exemption here \
                 and give it the standard docs/policy surface (#3148)"
            );
        }
    }
}
