//! `glob` — find files matching a glob pattern. Shells to `fd` when present,
//! falls back to `find` otherwise.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use crate::registry::Tool;

const MAX_RESULTS: usize = 500;

pub struct Glob {
    /// Whether results carry the code-map footer. Off for the embedded sweeps
    /// in `gather_context` — see [`crate::grep::Grep`].
    footer: bool,
}

impl Glob {
    /// The model-facing tool: footer on. A glob is a file-name match, not a
    /// symbol hunt, so it never carries the `graph_query` tip (that rides
    /// symbol-shaped `grep` searches — see [`crate::code_map::is_symbol_shaped`]).
    pub fn with_code_map() -> Self {
        Self { footer: true }
    }

    /// Footer-less instance for embedded use.
    pub fn bare() -> Self {
        Self { footer: false }
    }
}

impl Default for Glob {
    /// Footer on — the standalone shape tests use.
    fn default() -> Self {
        Self::with_code_map()
    }
}

#[async_trait]
impl Tool for Glob {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Returns relative paths from \
                          workspace root. When the code-graph index exists, results carry a \
                          code-map footer (matched files' symbols and import edges)."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs)" },
                    "path": { "type": "string", "description": "Subdirectory to search (default: workspace root)" }
                },
                "required": ["pattern"]
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let pattern = match crate::input::required_str(input, "pattern") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let search_path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        // Confine `path` to the workspace root — `Path::join` would otherwise
        // let an absolute or `../..` path escape it. `.`/empty resolve to the
        // root itself, so existing default-scoped calls keep working.
        let search_dir = match crate::resolve_within_root(root, search_path) {
            Some(p) => p,
            None => {
                return ToolOutput::error(format!("path `{search_path}` escapes workspace root"));
            }
        };
        // A typo'd `path` is caught here, deterministically, rather than by
        // parsing backend stderr: fd and find phrase a missing search dir
        // differently, and find's phrasing ("No such file or directory") is
        // byte-identical to the noise an entry vanishing mid-walk produces.
        // With existence settled up front, backend_failure can treat every
        // vanished-entry complaint as the race it is.
        if !search_dir.is_dir() {
            return ToolOutput::error(format!("search path `{search_path}` is not a directory"));
        }
        // Results are rendered relative to the (canonical) workspace root, per
        // this tool's advertised contract.
        let canon_root = root.canonicalize().ok();

        let keep_git = names_git_dir(search_path, pattern);
        let fd = fd_command(&search_dir, pattern, keep_git);
        // `run_captured` sets `kill_on_drop` (a cancelled turn must not leave a
        // full-tree walk burning IO) and bounds the wait.
        match crate::exec::run_captured(fd, crate::grep::SEARCH_TIMEOUT_SECS).await {
            crate::exec::Captured::TimedOut => ToolOutput::error(format!(
                "fd timed out after {}s — narrow the search with a `path` filter",
                crate::grep::SEARCH_TIMEOUT_SECS
            )),
            crate::exec::Captured::Done(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                if text.is_empty() {
                    // A backend failure must surface as an error, not be
                    // swallowed as "(no files found)" — same rule as grep's
                    // pattern guard: a typo'd `path` or malformed glob makes
                    // fd exit non-zero with empty stdout, and reporting that
                    // as an empty tree sends the agent off recreating files
                    // that exist.
                    if let Some(message) = backend_failure("fd", &output) {
                        return ToolOutput::error(message);
                    }
                    return ToolOutput::Ok {
                        content: "(no files found)".into(),
                    };
                }
                let content = render_matches(&text, canon_root.as_deref());
                ToolOutput::Ok {
                    content: with_code_map(content, root, self.footer),
                }
            }
            crate::exec::Captured::Unavailable => {
                // fd not installed — fall back to find (same pinned dir).
                let find = find_command(&search_dir, pattern, keep_git);
                // Same cancellation backstop as the fd arm above.

                match crate::exec::run_captured(find, crate::grep::FALLBACK_SEARCH_TIMEOUT_SECS)
                    .await
                {
                    crate::exec::Captured::TimedOut => ToolOutput::error(format!(
                        "find timed out after {}s — narrow the search with a `path` filter",
                        crate::grep::FALLBACK_SEARCH_TIMEOUT_SECS
                    )),
                    crate::exec::Captured::Done(output) => {
                        let text = String::from_utf8_lossy(&output.stdout);
                        if text.is_empty() {
                            // Same backend-failure guard as the fd arm.
                            if let Some(message) = backend_failure("find", &output) {
                                return ToolOutput::error(message);
                            }
                            ToolOutput::Ok {
                                content: "(no files found)".into(),
                            }
                        } else {
                            let content = render_matches(&text, canon_root.as_deref());
                            ToolOutput::Ok {
                                content: with_code_map(content, root, self.footer),
                            }
                        }
                    }
                    crate::exec::Captured::Unavailable => {
                        ToolOutput::error("find failed: neither `fd` nor `find` is available")
                    }
                }
            }
        }
    }
}

/// Build the `fd` fast path for `pattern` under `search_dir`.
///
/// `--glob` makes the pattern a glob rather than a regex, so `*.rs` matches
/// the way `find -name "*.rs"` does.
///
/// `--hidden` is not optional politeness: `fd` skips dot-prefixed entries by
/// default, so `.github/workflows/*.yml`, `.cargo/config.toml` and every
/// dotfile in the tree came back as "(no files found)" — which an agent reads
/// as proof the file does not exist and then recreates it. The `find`
/// fallback never had that blind spot, so the two backends also disagreed
/// depending on what happened to be installed. `--exclude .git` is the other
/// half: once hidden entries are in scope, `.git`'s object store is thousands
/// of files that are never what was asked for, and the same prune is applied
/// to the `find` fallback ([`find_command`]) so both backends answer alike.
/// `.gitignore` pruning stays on — that one is a feature, and it is the
/// documented difference between the two backends.
///
/// The `.git` prune is CONDITIONAL, and the condition is the whole point:
/// it applies only while the caller has not named `.git` themselves
/// ([`names_git_dir`]). Unconditional, it re-created for `.git` the exact
/// defect `--hidden` was added to fix one paragraph above — an agent asking
/// `glob ".git/refs/heads/*"` was told "(no files found)", which is not a
/// refusal it can act on but a false statement about the tree. A `fix-git`
/// bench trial burned eight calls on that lie, and the tell was the
/// asymmetry it produced: `{path: ".git/refs/heads", pattern: "*"}` worked,
/// because a prune keyed on a path COMPONENT never fires when the walk
/// starts below that component. `read_file` on the very path glob denied
/// returned its contents, so the two read tools disagreed about what
/// existed. When the caller does reach in, `objects` is still pruned — that
/// is the bulk subtree the perf argument was ever really about, and it is
/// never what someone asking about `.git` wants.
/// A zero-match result that is really a backend failure: non-zero exit with
/// nothing on stdout and a reason on stderr. `None` for the benign shape
/// (a genuine empty result, or noise-free non-zero exits from unreadable
/// subtrees that printed nothing).
fn backend_failure(program: &str, output: &std::process::Output) -> Option<String> {
    if output.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Per-entry traversal noise is benign: a walk that brushes an unreadable
    // directory (systemd-private tempdirs on a shared /tmp) or an entry that
    // vanishes between enumeration and stat (another process cleaning its
    // tempdir) makes find/fd exit non-zero while the empty result is still
    // the honest answer — the same tolerance grep's pattern guard applies.
    // "No such file or directory" can only be that race here: the search dir
    // itself was verified in `execute` before the backend ran. Only a
    // complaint outside these shapes makes the empty result a lie worth
    // surfacing (malformed pattern, unlaunchable backend).
    let first = stderr.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.contains("Permission denied")
            && !line.contains("Operation not permitted")
            && !line.contains("No such file or directory")
    })?;
    Some(format!("{program} error: {first}"))
}

/// Whether the caller explicitly reached into `.git`, in either half of the
/// request. A `.git` component in the pinned `path` or anywhere in the glob
/// is an explicit ask, and an explicit ask is answered rather than pruned.
///
/// Matched on path COMPONENTS, not as a substring, so a file that merely
/// contains the letters (`.gitignore`, `dot.github`) does not disable the
/// prune for an ordinary walk.
/// Escape the glob metacharacters in a literal path prefix.
///
/// Both backends anchor a slash-carrying pattern by prepending the search
/// directory, and both then interpret the result as a glob — so a workspace
/// checked out under a directory whose name contains `[`, `*` or `?` would
/// have that name reinterpreted as a character class or wildcard and match
/// nothing. The prefix is literal text by construction; this keeps it that
/// way. `fd`'s glob engine and `find`'s `fnmatch` both honour backslash
/// escapes.
fn escape_glob_meta(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for ch in literal.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn names_git_dir(search_path: &str, pattern: &str) -> bool {
    let component = |s: &str| {
        s.split(['/', '\\'])
            .any(|part| part == ".git" || part.starts_with(".git/"))
    };
    component(search_path) || component(pattern)
}

fn fd_command(search_dir: &std::path::Path, pattern: &str, keep_git: bool) -> Command {
    let mut fd = Command::new("fd");
    fd.arg("--glob");
    fd.arg("--hidden");
    // See the doc above: prune `.git` wholesale only while nobody asked
    // about it; once they have, prune just the bulk object store.
    fd.arg("--exclude")
        .arg(if keep_git { "objects" } else { ".git" });
    fd.arg("--type").arg("f");
    fd.arg("--color").arg("never");
    // Single-threaded, for the same reason `grep` passes `--sort path`: `fd`
    // walks in PARALLEL, so the same glob over the same unchanged tree
    // answers in a different order every call — and because `--max-results`
    // stops the walk early, a capped search returns a different SET too.
    // `stella_core::loop_detect` proves a turn is stuck by finding
    // byte-identical tool output, so a search that never repeats itself is a
    // search it can never catch looping. Sorting the lines afterwards would
    // fix the order and quietly leave the set arbitrary, which reads as a fix
    // and is not one — the walk itself has to be deterministic.
    fd.arg("--threads").arg("1");
    fd.arg("--max-results").arg(MAX_RESULTS.to_string());
    // `fd --glob` matches the FILE NAME. A slash-carrying pattern therefore
    // matched nothing at all — `src/**/*.ts` and `.git/refs/heads/*` both
    // came back "(no files found)" on every machine with `fd` installed,
    // while the `find` fallback answered them correctly through its `-path`
    // branch. That is the same two-backends-disagree defect the module doc
    // records for hidden files, in the direction that reads as proof the
    // files do not exist. (`**/*.rs` survived by accident: a leading `**/`
    // leaves a bare basename glob behind.)
    //
    // `--full-path` switches to path matching, and the pattern is then
    // anchored at the search directory exactly as `find_command` anchors
    // its `-path` — otherwise `src/*.ts` would also match `vendor/src/*.ts`.
    // Unlike `find`, `fd`'s engine implements `**`, so no collapsing is
    // needed here and the anchored form is the more precise of the two.
    let matched = if pattern.contains('/') {
        fd.arg("--full-path");
        format!(
            "{}/{pattern}",
            escape_glob_meta(&search_dir.to_string_lossy())
        )
    } else {
        pattern.to_string()
    };
    fd.arg("--").arg(&matched).arg(search_dir);
    crate::subprocess_env::scrub_sensitive_env(&mut fd);
    fd.stdout(std::process::Stdio::piped());
    fd.stderr(std::process::Stdio::piped());
    fd
}

/// Build the `find` fallback for `pattern` under `search_dir`.
///
/// `-name` matches basenames only, so a slash-carrying glob — including the
/// schema's own `**/*.rs` example — matched nothing through it. Those
/// patterns go through `-path` against the full path instead, prefixed with
/// the search dir (the strings `find` prints all start with it). `find`'s
/// glob has no `**`, but its `*` already crosses `/` (fnmatch without
/// FNM_PATHNAME), so `**` collapses to `*`: `**/*.rs` → `<dir>/*.rs`,
/// `src/**/*.ts` → `<dir>/src/*.ts`. The same slash-crossing `*` makes a
/// mid-pattern `a/**/b.rs` also match `a/xb.rs` — in a search tool a
/// slightly wide match beats silently missing files.
///
/// `.git` is pruned to match [`fd_command`]'s `--exclude`, under the same
/// condition and for the same reason (see that function): without any prune
/// the fallback answered `**/*` with thousands of loose-object paths, and the
/// two backends disagreed wildly depending on which was installed; with an
/// unconditional one, a caller who explicitly asked about `.git` was told it
/// was empty. `keep_git` narrows the prune to `objects`. The explicit
/// `-print` is required once `-prune -o` is in the expression — `find`'s
/// implicit print would otherwise apply to the whole expression and emit the
/// pruned directory itself.
fn find_command(search_dir: &std::path::Path, pattern: &str, keep_git: bool) -> Command {
    let mut find = Command::new("find");
    find.arg(search_dir);
    // Same conditional prune as `fd_command`, so the two backends still
    // answer alike — the parity the module doc requires of every prune.
    find.arg("-name")
        .arg(if keep_git { "objects" } else { ".git" })
        .arg("-prune")
        .arg("-o");
    find.arg("-type").arg("f");
    if pattern.contains('/') {
        let mut translated = pattern.replace("**/", "*");
        while translated.contains("**") {
            translated = translated.replace("**", "*");
        }
        find.arg("-path").arg(format!(
            "{}/{translated}",
            escape_glob_meta(&search_dir.to_string_lossy())
        ));
    } else {
        find.arg("-name").arg(pattern);
    }
    find.arg("-print");
    crate::subprocess_env::scrub_sensitive_env(&mut find);
    find.stdout(std::process::Stdio::piped());
    find.stderr(std::process::Stdio::piped());
    find
}

/// Render the backend's match lines as workspace-relative paths, capped at
/// [`MAX_RESULTS`] — and SAY SO when the cap bit. A silently truncated file
/// list is worse than a short one: the agent reads the absence of a path as
/// proof the file does not exist and goes on to recreate it. (`fd` is also
/// given `--max-results`, so an exactly-full list may or may not have had
/// more behind it; "first N" is the honest phrasing either way — the same
/// wording `grep` uses for the same reason.)
fn render_matches(text: &str, canon_root: Option<&std::path::Path>) -> String {
    let paths: Vec<String> = text
        .lines()
        .take(MAX_RESULTS)
        .map(|l| relativize(l, canon_root))
        .collect();
    let capped = paths.len() == MAX_RESULTS;
    let mut content = paths.join("\n");
    if capped {
        content.push_str(&format!(
            "\n... (showing first {MAX_RESULTS} files — narrow `pattern` or `path` for the rest)"
        ));
    }
    content
}

/// Append the code-map footer for these matched paths, when the footer is
/// enabled and the graph has one to give (see [`crate::code_map`]). The
/// `graph_query` tip never rides a glob (`show_tip = false`) — it belongs on
/// symbol-shaped `grep` searches. Bound separately from the `if let` so the
/// `content.lines()` borrow ends before `content` is mutated.
fn with_code_map(mut content: String, root: &std::path::Path, footer: bool) -> String {
    if !footer {
        return content;
    }
    let map = crate::code_map::for_files(root, content.lines(), false);
    if let Some(map) = map {
        content.push_str(&map);
    }
    content
}

/// Render an absolute match path relative to the workspace root, honoring the
/// tool's advertised "relative paths from workspace root" contract. Falls back
/// to the original string if the root is unknown or the path lies outside it.
fn relativize(line: &str, canon_root: Option<&std::path::Path>) -> String {
    match canon_root {
        Some(root) => std::path::Path::new(line)
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| line.to_string()),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_files_matching_pattern() {
        let dir = std::env::temp_dir();
        let subdir = format!("stella_glob_{}", std::process::id());
        tokio::fs::create_dir_all(dir.join(&subdir)).await.unwrap();
        tokio::fs::write(dir.join(&subdir).join("a.rs"), "fn main(){}")
            .await
            .unwrap();
        tokio::fs::write(dir.join(&subdir).join("b.txt"), "hello")
            .await
            .unwrap();

        // Use a glob pattern (not regex) so both `fd` and `find -name`
        // handle it identically across platforms.
        let result = Glob::default()
            .execute(
                &serde_json::json!({"pattern": "*.rs", "path": &subdir}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("a.rs"), "expected a.rs in: {content}");
                assert!(
                    !content.contains("b.txt"),
                    "should not contain b.txt: {content}"
                );
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        let _ = tokio::fs::remove_dir_all(dir.join(&subdir)).await;
    }

    #[tokio::test]
    async fn results_carry_a_code_map_footer_when_an_index_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("lib.rs"), "pub fn greet() {}\n").expect("write");
        let db = crate::graph::graph_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(dir.path(), &db).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();

        let result = Glob::default()
            .execute(&serde_json::json!({"pattern": "*.rs"}), dir.path())
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("lib.rs"), "the file list stays: {content}");
                assert!(content.contains("code map"), "footer attached: {content}");
                assert!(
                    content.contains("function greet:1"),
                    "maps the listed file's symbols: {content}"
                );
            }
            ToolOutput::Error { message, .. } => panic!("expected files, got: {message}"),
        }
    }

    /// A truncated file list must say it was truncated — an agent that reads
    /// a silently-capped list as complete concludes a file does not exist.
    #[test]
    fn the_result_cap_is_announced_and_invisible_below_it() {
        let short = "a.rs\nb.rs";
        assert_eq!(render_matches(short, None), short);

        let many: String = (0..MAX_RESULTS + 10)
            .map(|i| format!("f{i}.rs\n"))
            .collect();
        let rendered = render_matches(&many, None);
        assert_eq!(rendered.lines().count(), MAX_RESULTS + 1, "capped + note");
        assert!(
            rendered.contains(&format!("showing first {MAX_RESULTS} files")),
            "{rendered}"
        );
    }

    /// Slash-carrying globs must not go through `-name` (basenames only) —
    /// through it the schema's own `**/*.rs` example matched nothing.
    #[test]
    fn find_fallback_routes_slash_globs_through_path_matching() {
        let dir = std::path::Path::new("/ws");
        let args = |pattern: &str| -> Vec<String> {
            find_command(dir, pattern, false)
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        // Basename patterns keep `-name` semantics (parity with `fd --glob`).
        assert!(args("*.rs").windows(2).any(|w| w == ["-name", "*.rs"]));
        // `**` collapses to `*` — `find`'s `*` already crosses `/`.
        assert!(
            args("**/*.rs")
                .windows(2)
                .any(|w| w == ["-path", "/ws/*.rs"])
        );
        assert!(
            args("src/**/*.ts")
                .windows(2)
                .any(|w| w == ["-path", "/ws/src/*.ts"])
        );
    }

    /// `fd` skips dot-prefixed entries unless told otherwise, so
    /// `.github/workflows/ci.yml` was invisible to `glob` on every machine
    /// with `fd` installed — and visible on every machine without it.
    #[test]
    fn fd_sees_hidden_files_and_prunes_git_like_the_find_fallback() {
        let args: Vec<String> = fd_command(std::path::Path::new("/ws"), "*.yml", false)
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--hidden".to_string()), "{args:?}");
        assert!(
            args.windows(2).any(|w| w == ["--exclude", ".git"]),
            "{args:?}"
        );
        // The pattern still arrives after `--`, so a leading-dash glob is not
        // parsed as an option.
        assert!(args.windows(2).any(|w| w == ["--", "*.yml"]), "{args:?}");
    }

    /// The prune fires on a path COMPONENT, so an ordinary walk still skips
    /// `.git` while a file that merely spells it does not disable it.
    #[test]
    fn names_git_dir_matches_components_not_substrings() {
        assert!(names_git_dir(".", ".git/refs/heads/*"));
        assert!(names_git_dir(".git/refs/heads", "*"));
        assert!(names_git_dir(".", ".git/**"));
        assert!(names_git_dir("nested/.git", "*"));

        assert!(!names_git_dir(".", "**/*.rs"));
        assert!(!names_git_dir(".", ".gitignore"));
        assert!(!names_git_dir(".", "**/.github/workflows/*.yml"));
        assert!(!names_git_dir("src", "*.rs"));
    }

    /// Narrowed, not dropped: a caller who asked about `.git` still does not
    /// get the loose-object store, and one who did not still gets the old
    /// wholesale prune.
    #[test]
    fn the_git_prune_narrows_to_objects_only_once_the_caller_asks() {
        let args = |keep_git: bool| -> Vec<String> {
            fd_command(std::path::Path::new("/ws"), ".git/refs/*", keep_git)
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert!(
            args(true).windows(2).any(|w| w == ["--exclude", "objects"]),
            "{:?}",
            args(true)
        );
        assert!(
            args(false).windows(2).any(|w| w == ["--exclude", ".git"]),
            "{:?}",
            args(false)
        );
    }

    /// Witness for the false negative a `fix-git` trial burned eight calls
    /// on: asking for a path inside `.git` answered "(no files found)" while
    /// `read_file` on the very same path returned its contents. Fails before
    /// the prune became conditional; passes after. Runs through `execute`, so
    /// whichever backend is installed has to agree.
    #[tokio::test]
    async fn glob_finds_an_explicitly_requested_git_metadata_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git/refs/heads")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join(".git/objects/ab")).expect("mkdir");
        std::fs::write(dir.path().join(".git/refs/heads/master"), "deadbeef\n").expect("write");
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/master\n").expect("write");
        std::fs::write(dir.path().join(".git/objects/ab/cdef"), "").expect("write");

        let out = Glob::bare()
            .execute(
                &serde_json::json!({ "pattern": ".git/refs/heads/*" }),
                dir.path(),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("glob must succeed: {out:?}");
        };
        assert!(
            content.contains(".git/refs/heads/master"),
            "an explicitly requested `.git` path must be found, not denied: {content}"
        );
        // The bulk object store stays pruned even then.
        assert!(!content.contains("objects/ab/cdef"), "{content}");
    }

    /// Witness for the wider defect the `.git` work uncovered: `fd --glob`
    /// matches the FILE NAME, so every slash-carrying pattern answered
    /// "(no files found)" wherever `fd` was installed — while the `find`
    /// fallback answered the same pattern correctly. `**/*.rs` survived by
    /// accident (a leading `**/` leaves a bare basename glob), which is why
    /// the existing coverage never caught it.
    #[tokio::test]
    async fn slash_globs_match_against_the_path_and_stay_anchored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/deep")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("vendor/src")).expect("mkdir");
        std::fs::write(dir.path().join("src/a.ts"), "").expect("write");
        std::fs::write(dir.path().join("src/deep/inner.ts"), "").expect("write");
        std::fs::write(dir.path().join("vendor/src/theirs.ts"), "").expect("write");
        std::fs::write(dir.path().join("stray.ts"), "").expect("write");

        let content = |pattern: &str| {
            let pattern = pattern.to_string();
            let root = dir.path().to_path_buf();
            async move {
                match Glob::bare()
                    .execute(&serde_json::json!({ "pattern": pattern }), &root)
                    .await
                {
                    ToolOutput::Ok { content } => content,
                    other => panic!("glob must succeed: {other:?}"),
                }
            }
        };

        let scoped = content("src/**/*.ts").await;
        assert!(scoped.contains("src/a.ts"), "{scoped}");
        assert!(scoped.contains("src/deep/inner.ts"), "{scoped}");
        // Anchored at the search root, exactly as the `find` branch anchors
        // its `-path`: a nested `vendor/src/` is a different directory.
        assert!(!scoped.contains("vendor/src/theirs.ts"), "{scoped}");
        assert!(!scoped.contains("stray.ts"), "{scoped}");

        // The unanchored form still reaches every depth.
        let everywhere = content("**/*.ts").await;
        assert!(everywhere.contains("stray.ts"), "{everywhere}");
        assert!(everywhere.contains("vendor/src/theirs.ts"), "{everywhere}");
    }

    /// A literal path prefix stays literal: both backends anchor by
    /// prepending the search directory and then read the result as a glob.
    #[test]
    fn glob_metacharacters_in_the_anchor_are_escaped() {
        assert_eq!(escape_glob_meta("/ws/plain"), "/ws/plain");
        assert_eq!(escape_glob_meta("/ws/a[1]/b"), "/ws/a\\[1\\]/b");
        assert_eq!(escape_glob_meta("/ws/re*po"), "/ws/re\\*po");
    }

    /// The other half: a walk that never mentions `.git` is unchanged.
    #[tokio::test]
    async fn an_ordinary_walk_still_skips_the_git_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git/refs/heads")).expect("mkdir");
        std::fs::write(dir.path().join(".git/refs/heads/master"), "x\n").expect("write");
        std::fs::write(dir.path().join("top.txt"), "").expect("write");

        let out = Glob::bare()
            .execute(&serde_json::json!({ "pattern": "**/*" }), dir.path())
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("glob must succeed: {out:?}");
        };
        assert!(content.contains("top.txt"), "{content}");
        assert!(!content.contains(".git/refs"), "{content}");
    }

    /// Both backends must answer a dotted path the same way, and neither may
    /// flood the result with `.git` internals.
    #[tokio::test]
    async fn the_find_fallback_finds_dotfiles_and_skips_the_git_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".github/workflows")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join(".git/objects")).expect("mkdir");
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "").expect("write");
        std::fs::write(dir.path().join(".git/objects/deadbeef.yml"), "").expect("write");

        let cmd = find_command(dir.path(), "**/*.yml", false);
        let out = match crate::exec::run_captured(cmd, crate::grep::FALLBACK_SEARCH_TIMEOUT_SECS)
            .await
        {
            crate::exec::Captured::Done(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
            other => panic!(
                "find must run: {}",
                matches!(other, crate::exec::Captured::TimedOut)
            ),
        };
        assert!(out.contains(".github/workflows/ci.yml"), "{out}");
        assert!(
            !out.contains(".git/objects"),
            "`.git` must be pruned: {out}"
        );
    }

    /// Run the real `find` through the fallback builder: `**/*.rs` matches at
    /// every depth, `src/**/*.ts` stays anchored under `src/`.
    #[tokio::test]
    async fn find_fallback_matches_recursive_globs_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/deep")).expect("mkdir");
        std::fs::write(dir.path().join("top.rs"), "").expect("write");
        std::fs::write(dir.path().join("src/deep/inner.rs"), "").expect("write");
        std::fs::write(dir.path().join("src/a.ts"), "").expect("write");
        std::fs::write(dir.path().join("src/deep/b.ts"), "").expect("write");
        std::fs::write(dir.path().join("stray.ts"), "").expect("write");

        let run = |pattern: &str| {
            let cmd = find_command(dir.path(), pattern, false);
            async move {
                match crate::exec::run_captured(cmd, crate::grep::FALLBACK_SEARCH_TIMEOUT_SECS)
                    .await
                {
                    crate::exec::Captured::Done(out) => {
                        String::from_utf8_lossy(&out.stdout).into_owned()
                    }
                    crate::exec::Captured::Unavailable => panic!("find must be installed"),
                    crate::exec::Captured::TimedOut => panic!("find timed out"),
                }
            }
        };

        let rs = run("**/*.rs").await;
        assert!(rs.contains("top.rs"), "{rs}");
        assert!(rs.contains("inner.rs"), "{rs}");
        assert!(!rs.contains(".ts"), "{rs}");

        let ts = run("src/**/*.ts").await;
        assert!(ts.contains("src/a.ts"), "{ts}");
        assert!(ts.contains("src/deep/b.ts"), "{ts}");
        assert!(!ts.contains("stray.ts"), "anchored under src/: {ts}");
    }

    #[tokio::test]
    async fn no_files_returns_ok() {
        let dir = std::env::temp_dir();
        let result = Glob::default()
            .execute(&serde_json::json!({"pattern": "*.nonexistent"}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("no files")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    #[test]
    fn a_backend_failure_is_an_error_not_an_empty_result() {
        use std::os::unix::process::ExitStatusExt;
        // Non-zero exit + empty stdout + a reason on stderr = failure.
        let failed = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"[fd error]: Search path 'srcc' is not a directory\n".to_vec(),
        };
        let message = backend_failure("fd", &failed).expect("failure must surface");
        assert!(
            message.contains("srcc"),
            "the reason must be named: {message}"
        );

        // A genuine empty result (exit 0) stays quiet, and so does a silent
        // non-zero exit (nothing to report).
        let empty = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(backend_failure("fd", &empty), None);
        let silent = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"   \n".to_vec(),
        };
        assert_eq!(backend_failure("fd", &silent), None);

        // Per-entry traversal noise stays quiet: a walk brushing another
        // user's unreadable directory, or an entry deleted mid-walk by a
        // concurrent process, exits non-zero while the empty result is
        // still the honest answer.
        let traversal_noise = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"find: '/tmp/systemd-private-abc': Permission denied\nfind: '/tmp/vanished_mid_walk': No such file or directory\n"
                .to_vec(),
        };
        assert_eq!(backend_failure("find", &traversal_noise), None);

        // ...but a real complaint after the noise still surfaces.
        let mixed = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"find: '/tmp/systemd-private-abc': Permission denied\nfind: paths must precede expression: `src[`\n"
                .to_vec(),
        };
        let message = backend_failure("find", &mixed).expect("real failure must surface");
        assert!(message.contains("precede expression"), "{message}");
    }

    #[tokio::test]
    async fn a_typod_search_path_is_a_named_error_before_any_backend_runs() {
        let dir = std::env::temp_dir();
        let result = Glob::default()
            .execute(
                &serde_json::json!({"pattern": "*.rs", "path": "srcc"}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("srcc"), "{message}");
                assert!(message.contains("not a directory"), "{message}");
            }
            ToolOutput::Ok { content } => panic!("expected error, got: {content}"),
        }
    }
}
