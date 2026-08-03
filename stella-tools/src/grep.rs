//! `grep` — search file contents with a regex. Shells to `rg` when present
//! for speed; falls back to the system `grep -rn` otherwise.
//!
//! Three independent caps bound what reaches the model, because
//! `MAX_RESULTS` alone does not: ripgrep's `--max-count` is per FILE, so a
//! recursive search over N matching files can emit 200·N lines, and a single
//! match inside a minified bundle or a base64 blob is one line of megabytes.
//! So: `MAX_COLUMNS` clips each match line (rg's own
//! `--max-columns-preview`, re-implemented in Rust for the `grep` fallback,
//! which has no such flag), `crate::exec::truncate_middle` caps the whole
//! buffered payload, and only then does the `MAX_RESULTS` line cap apply —
//! that order is what keeps the "showing first 200 matches" note true.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use crate::registry::Tool;

const MAX_RESULTS: usize = 200;

/// Ceiling on `-A`/`-B`/`-C`. Context multiplies the result: at `MAX_RESULTS`
/// matches, one extra context line each side is 3x the payload, so an
/// unclamped value is a way to blow the whole line budget on a single search.
/// Twenty is more than any human reads around a hit and still leaves the
/// `MAX_RESULTS` cap doing real work.
const MAX_CONTEXT_LINES: u64 = 20;

/// What shape of answer the caller wants back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// `path:LINE:text` per match — today's behaviour, and the default.
    Content,
    /// Matching file paths only (`rg -l`). The cheap way to ask "where does
    /// this live" without paying for every hit in a file that has hundreds.
    FilesWithMatches,
    /// `path:COUNT` per file (`rg -c`).
    Count,
}

impl OutputMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "content" => Some(Self::Content),
            "files_with_matches" => Some(Self::FilesWithMatches),
            "count" => Some(Self::Count),
            _ => None,
        }
    }

    /// What one line of this mode's output *is*, for the truncation note. With
    /// context lines on, a "matches" count is simply false — the cap counts
    /// lines, and most of them did not match.
    fn unit(self, has_context: bool) -> &'static str {
        match self {
            Self::Content if has_context => "lines",
            Self::Content => "matches",
            Self::FilesWithMatches => "files",
            Self::Count => "files",
        }
    }
}

/// The parsed, clamped, already-validated search request. Built once by
/// [`SearchOptions::parse`] so both the rg arm and the `grep` fallback read
/// the same decisions rather than re-deriving them from the raw JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchOptions {
    mode: OutputMode,
    before: u64,
    after: u64,
    case_insensitive: bool,
}

impl SearchOptions {
    /// Reject rather than ignore. A caller that asked for context in
    /// `files_with_matches` mode has a wrong model of the tool, and silently
    /// dropping the field teaches it that the field did nothing — the next
    /// call makes the same mistake. Naming the conflict fixes it in one turn.
    fn parse(input: &Value) -> Result<Self, String> {
        let mode = match input.get("output_mode").and_then(|v| v.as_str()) {
            None => OutputMode::Content,
            Some(raw) => OutputMode::parse(raw).ok_or_else(|| {
                format!(
                    "`output_mode` must be one of \"content\", \"files_with_matches\", \
                     \"count\" — got {raw:?}"
                )
            })?,
        };

        // `context` is the both-sides shorthand; the directional fields win
        // over it when both are given, matching `rg`'s own precedence.
        let symmetric = clamped_context(input, "context")?;
        let before = clamped_context(input, "context_before")?.or(symmetric);
        let after = clamped_context(input, "context_after")?.or(symmetric);

        if mode != OutputMode::Content && (before.is_some() || after.is_some()) {
            return Err(format!(
                "context lines are only meaningful with `output_mode: \"content\"` — \
                 drop the context field or switch modes (got {:?})",
                match mode {
                    OutputMode::FilesWithMatches => "files_with_matches",
                    _ => "count",
                }
            ));
        }

        Ok(Self {
            mode,
            before: before.unwrap_or(0),
            after: after.unwrap_or(0),
            case_insensitive: input
                .get("case_insensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    fn has_context(&self) -> bool {
        self.before > 0 || self.after > 0
    }
}

/// One context knob, validated and clamped to [`MAX_CONTEXT_LINES`]. A
/// wrong-typed value is reported as wrong-typed rather than as absent —
/// `.as_u64()` alone collapses "missing" and "not a number" into the same
/// `None`, which is the defect #1267 is about; there is no reason to add a
/// fresh instance of it here.
fn clamped_context(input: &Value, field: &str) -> Result<Option<u64>, String> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) => Ok(Some(n.min(MAX_CONTEXT_LINES))),
            None => Err(format!(
                "`{field}` must be a non-negative integer — got {v}"
            )),
        },
    }
}

/// Wall-clock ceiling on the ripgrep pass. rg prunes with .gitignore and is
/// I/O-bound, so a whole-workspace search that has not answered in a minute is
/// wedged (a stalled network mount, a pathological backtracking pattern), not
/// merely busy — and every second past that is a turn the user cannot cancel
/// out of any faster.
pub(crate) const SEARCH_TIMEOUT_SECS: u64 = 60;

/// The `grep`/`find` fallbacks get a longer leash: they do no ignore-file
/// pruning, so a legitimately slow-but-progressing walk of a large tree is
/// plausible where it would not be for rg/fd.
pub(crate) const FALLBACK_SEARCH_TIMEOUT_SECS: u64 = 120;

/// Per-line column cap for a match line. The issue proposed 300 and 400 in
/// two different places; 400 is the choice — wide enough for a real source
/// line with indentation, narrow enough that 200 of them cannot be more than
/// ~80 KB before the payload cap even applies.
const MAX_COLUMNS: usize = 400;

/// The `grep` fallback's stand-in for rg's `--max-columns-preview`: clip
/// every line to `MAX_COLUMNS` bytes with the crate's loud elision marker.
/// Short lines pass through byte-identically, so ordinary results are
/// untouched. Char-boundary-safe — byte slicing would panic mid-UTF-8.
fn cap_columns(text: &str) -> String {
    if !text.lines().any(|line| line.len() > MAX_COLUMNS) {
        return text.to_string();
    }
    text.lines()
        .map(|line| {
            if line.len() <= MAX_COLUMNS {
                line.to_string()
            } else {
                let head = crate::exec::truncate_preview(line, MAX_COLUMNS);
                let elided = line.len() - head.len();
                format!("{head}[… {elided} bytes elided …]")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does this rg/grep stderr describe a broken *pattern* (bad regex or a flag
/// the pattern got mistaken for) rather than benign per-file walk warnings?
/// Both tools exit 2 either way, so the message text is the only signal —
/// read per line, anchored right after the `rg:`/`grep:` prefix: pattern
/// failures put the diagnostic there (`regex parse error`, `error: …`,
/// `invalid …`, `unbalanced …`), while per-path IO warnings put a path
/// there (`rg: <path>: <os reason>`). A bare substring scan mistook any
/// warning whose *path* contained a keyword (`…/invalid/…`) for a broken
/// pattern, turning a legitimate zero-match walk into a failure.
fn is_pattern_error(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        let rest = line
            .strip_prefix("rg:")
            .or_else(|| line.strip_prefix("grep:"))
            .unwrap_or(&line)
            .trim_start();
        rest.starts_with("regex parse error")
            || ["error:", "invalid", "unbalanced", "unmatched"]
                .iter()
                .any(|kw| rest.starts_with(kw))
    })
}

/// The most informative single line of an rg/grep error, for a compact tool
/// message (the full multi-line dump is noise to the model).
fn first_error_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("invalid") || l.contains("unbalanced")
        })
        .or_else(|| stderr.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("search failed")
        .to_string()
}

/// The file path one result line refers to, or `None` for a line that names
/// no file.
///
/// Three output shapes reach this, and only the first one splits cleanly on
/// `:`. A **match** is `path:LINE:text`. A **context** line — the thing
/// `-A`/`-B`/`-C` adds — is `path-LINE-text`, because ripgrep marks a
/// non-matching line by changing the field separator rather than by omitting
/// the fields. A **group separator** between non-contiguous context runs is a
/// bare `--`.
///
/// So the pre-context `l.split(':').next()` handed a whole context line to the
/// graph as if it were a path, and because the search dir is absolute,
/// `is_absolute()` agreed and the lookup silently missed — the footer would
/// have quietly emptied out exactly when context was most useful.
///
/// `:` is probed before `-` so an ordinary match wins on its real separator
/// even when the *path* contains a `-N-` run. A context line on such a path
/// (`/a/x-12-y.rs-5-text`) is still ambiguous and resolves short; that is
/// best-effort by construction, as it was before.
fn path_of_result_line(line: &str) -> Option<&str> {
    if line == "--" || line.is_empty() {
        return None;
    }
    for &sep in b":-" {
        if let Some(idx) = field_break(line, sep) {
            return Some(&line[..idx]);
        }
    }
    // `-l` emits a bare path; `-c` emits `path:COUNT`, whose `:` is not a
    // field break because no second separator follows the digits.
    line.split(':').next()
}

/// Drop `path:0` rows from a recursive `grep -c` result, keeping every row
/// that reports at least one match. Split from the right: a path may itself
/// contain a `:`, and only the final field is the count.
fn drop_zero_counts(text: &str) -> String {
    text.lines()
        .filter(|line| !matches!(line.rsplit_once(':'), Some((_, count)) if count.trim() == "0"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Index of the first `<sep><digits><sep>` field break in `line`, if any.
fn field_break(line: &str, sep: u8) -> Option<usize> {
    let bytes = line.as_bytes();
    bytes.iter().enumerate().find_map(|(i, b)| {
        if *b != sep {
            return None;
        }
        let rest = &bytes[i + 1..];
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        (digits > 0 && rest.get(digits) == Some(&sep)).then_some(i)
    })
}

/// The code-map footer for these result lines (see [`crate::code_map`]), or
/// `None` when disabled or nothing maps. A directory search prints an absolute
/// path before the first field break; a single-file search omits the filename
/// entirely (rg and grep both do), so there the search target itself is the
/// mapped file. Non-path prefixes simply miss in the graph — best-effort by
/// construction. `show_tip` appends the `graph_query` pointer, set by the
/// caller from whether the *pattern* is symbol-shaped.
fn code_map_for(
    enabled: bool,
    show_tip: bool,
    search_dir: &std::path::Path,
    root: &std::path::Path,
    lines: &[&str],
) -> Option<String> {
    if !enabled {
        return None;
    }
    if search_dir.is_file() {
        let target = search_dir.to_string_lossy();
        return crate::code_map::for_files(root, [target.as_ref()], show_tip);
    }
    crate::code_map::for_files(
        root,
        lines
            .iter()
            .filter_map(|l| path_of_result_line(l))
            .filter(|p| std::path::Path::new(p).is_absolute()),
        show_tip,
    )
}

/// The empty-result answer, carrying the `graph_query` pointer when the
/// pattern was a symbol hunt and the workspace has an index.
///
/// A symbol-shaped search that matched nothing is the single strongest moment
/// to steer (#896): the agent has just been told the text does not exist
/// anywhere, and the graph is the one surface that can still answer — under a
/// different spelling, or by pointing at the definition the text search
/// missed. Before this the answer was a bare "(no matches)", so the steer that
/// the issue calls one-shot never even fired there.
fn no_matches(enabled: bool, show_tip: bool, root: &std::path::Path) -> ToolOutput {
    let mut content = String::from("(no matches)");
    if enabled && show_tip && matches!(crate::graph::graph_available(root), Ok(true)) {
        content.push_str("\n\n");
        content.push_str(crate::code_map::GRAPH_QUERY_TIP);
    }
    ToolOutput::Ok { content }
}

pub struct Grep {
    /// Whether results carry the code-map footer. Off for the embedded sweeps
    /// in `gather_context`, whose packs run their own graph queries — a
    /// per-section footer there is duplication, not context.
    footer: bool,
}

impl Grep {
    /// The model-facing tool: footer on. The `graph_query` tip rides a footer
    /// whenever the search pattern is symbol-shaped — a per-call decision, no
    /// shared state (see [`crate::code_map::is_symbol_shaped`]).
    pub fn with_code_map() -> Self {
        Self { footer: true }
    }

    /// Footer-less instance for embedded use.
    pub fn bare() -> Self {
        Self { footer: false }
    }
}

impl Default for Grep {
    /// Footer on — the standalone shape tests use.
    fn default() -> Self {
        Self::with_code_map()
    }
}

#[async_trait]
impl Tool for Grep {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description: "Search file contents with a regex. Returns matching file:line:text, with context lines around each hit when asked. Shells to ripgrep when available. When the code-graph index exists, matches carry a code-map footer (matched files' symbols and import edges).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for" },
                    "path": { "type": "string", "description": "Subdirectory to search (default: workspace root)" },
                    "glob": { "type": "string", "description": "Restrict to files matching this glob (e.g. *.rs)" },
                    "type": { "type": "string", "description": "Restrict to a ripgrep file type (e.g. rust, py, ts). Ignored by the grep fallback when ripgrep is unavailable." },
                    "case_insensitive": { "type": "boolean", "description": "Match case-insensitively (default: false)" },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "content (default) returns file:line:text; files_with_matches returns matching paths only; count returns file:count. Use files_with_matches to locate something cheaply before reading it."
                    },
                    "context": { "type": "integer", "description": "Lines of context on BOTH sides of each match (like grep -C). Max 20. Only valid with output_mode content." },
                    "context_before": { "type": "integer", "description": "Lines of context before each match (like grep -B). Max 20. Overrides `context`." },
                    "context_after": { "type": "integer", "description": "Lines of context after each match (like grep -A). Max 20. Overrides `context`." }
                },
                "required": ["pattern"]
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let pattern = match input.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `pattern`".into(),
                };
            }
        };
        let search_path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let glob_filter = input.get("glob").and_then(|v| v.as_str());
        let type_filter = input.get("type").and_then(|v| v.as_str());
        let opts = match SearchOptions::parse(input) {
            Ok(o) => o,
            Err(message) => return ToolOutput::Error { message },
        };
        // The `graph_query` pointer rides a footer only when the pattern reads
        // like a definition/reference hunt — the case the graph serves better.
        let show_tip = self.footer && crate::code_map::is_symbol_shaped(pattern);

        // Confine `path` to the workspace root — `Path::join` would otherwise
        // let an absolute or `../..` path escape it. `.`/empty resolve to the
        // root itself, so existing default-scoped calls keep working.
        let search_dir = match crate::resolve_within_root(root, search_path) {
            Some(p) => p,
            None => {
                return ToolOutput::Error {
                    message: format!("path `{search_path}` escapes workspace root"),
                };
            }
        };

        // Try ripgrep first — it's the fast path.
        let mut rg = Command::new("rg");
        rg.arg("--line-number")
            .arg("--no-heading")
            .arg("--color")
            .arg("never");
        rg.arg("--max-count").arg(MAX_RESULTS.to_string());
        // `--max-count` is PER FILE, so it is not a bound on the result at
        // all; the column cap is what keeps one minified line from becoming
        // the whole tool result. `--max-columns-preview` keeps the head of
        // the line instead of replacing it with rg's bare "omitted" notice.
        rg.arg("--max-columns")
            .arg(MAX_COLUMNS.to_string())
            .arg("--max-columns-preview");
        // `rg` skips dot-prefixed entries by default, so a search under
        // `.github/`, `.cargo/` or any dotfile came back "(no matches)" —
        // which an agent reads as "that code does not exist". The `grep`
        // fallback never had that blind spot, so the two backends also
        // disagreed depending on what was installed. `.git` is excluded
        // explicitly (its object store is never the answer), matching the
        // `--exclude-dir=.git` given to the fallback below. `.gitignore`
        // pruning stays on: that difference between the backends is a
        // feature, and it is what keeps `target/`/`node_modules` out.
        rg.arg("--hidden").arg("--glob").arg("!.git/");
        if let Some(g) = glob_filter {
            rg.arg("--glob").arg(g);
        }
        if let Some(t) = type_filter {
            rg.arg("--type").arg(t);
        }
        if opts.case_insensitive {
            rg.arg("--ignore-case");
        }
        match opts.mode {
            OutputMode::Content => {
                // Asymmetric flags rather than `-C`, since the two sides are
                // independently settable and `-C` would flatten them.
                if opts.before > 0 {
                    rg.arg("--before-context").arg(opts.before.to_string());
                }
                if opts.after > 0 {
                    rg.arg("--after-context").arg(opts.after.to_string());
                }
            }
            OutputMode::FilesWithMatches => {
                rg.arg("--files-with-matches");
            }
            OutputMode::Count => {
                // `--count-matches`, not `--count`: the question "how many
                // times does this appear" is the one worth paying for, and
                // rg's bare `--count` answers the different question "how many
                // LINES contain it", which undercounts every line with two
                // hits.
                rg.arg("--count-matches");
            }
        }
        // `-e <pattern>` (not a bare positional) so a pattern that begins
        // with `-` — the everyday `->`, `--flag`, `-n` — is treated as the
        // search string, not parsed as an rg option.
        rg.arg("-e").arg(pattern).arg(&search_dir);
        crate::subprocess_env::scrub_sensitive_env(&mut rg);
        rg.stdout(std::process::Stdio::piped());
        rg.stderr(std::process::Stdio::piped());
        // `run_captured` sets `kill_on_drop`: a cancelled turn (Esc) drops this
        // future mid-`output()`, tokio keeps the child running by default, and
        // a recursive walk of a large tree is exactly the thing that must not
        // keep burning IO after the user stopped asking. It also bounds the
        // wait, so a walk that wedges cannot hold the turn open either.
        match crate::exec::run_captured(rg, SEARCH_TIMEOUT_SECS).await {
            crate::exec::Captured::TimedOut => ToolOutput::Error {
                message: format!(
                    "rg timed out after {SEARCH_TIMEOUT_SECS}s — narrow the search with a \
                     `path` or `glob` filter"
                ),
            },
            crate::exec::Captured::Done(output) => {
                // Byte cap BEFORE the line cap: truncating after `take` would
                // make the "showing first 200 matches" note below a lie, and
                // rg's per-file `--max-count` means the buffer can hold far
                // more than 200 lines.
                let text = crate::exec::truncate_middle(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                );
                if !text.is_empty() {
                    // Matches (possibly partial — rg still exits 2 if some
                    // path in a recursive walk was unreadable). Never drop
                    // real results over a benign per-file walk warning.
                    let lines: Vec<&str> = text.lines().take(MAX_RESULTS).collect();
                    let mut result = lines.join("\n");
                    if lines.len() == MAX_RESULTS {
                        // The unit has to follow the mode: with context on,
                        // most of these lines did not match, and calling them
                        // "matches" would misreport the search's own reach.
                        let unit = opts.mode.unit(opts.has_context());
                        result.push_str(&format!("\n... (showing first {MAX_RESULTS} {unit})"));
                    }
                    if let Some(map) =
                        code_map_for(self.footer, show_tip, &search_dir, root, &lines)
                    {
                        result.push_str(&map);
                    }
                    return ToolOutput::Ok { content: result };
                }
                // No results. Exit 2 with a *pattern* error (bad regex, bad
                // flag) must surface — the old code masked it as
                // "(no matches)". A plain exit 2 from unreadable directories
                // during the walk is benign and stays "(no matches)".
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.code() == Some(2) && is_pattern_error(&stderr) {
                    return ToolOutput::Error {
                        message: format!("rg error: {}", first_error_line(&stderr)),
                    };
                }
                no_matches(self.footer, show_tip, root)
            }
            crate::exec::Captured::Unavailable => {
                // rg not installed — fall back to grep
                let mut grep = Command::new("grep");
                grep.arg("-rn").arg("--color=never");
                // Parity with the rg arm's `--glob !.git/`.
                grep.arg("--exclude-dir=.git");
                if let Some(g) = glob_filter {
                    grep.arg("--include").arg(g);
                }
                // `--type` has no `grep` equivalent — there is no file-type
                // database to consult — so it is the one field this backend
                // cannot honour. The schema says so rather than letting the
                // two backends disagree silently, which is the trap the
                // hidden-file divergence already sprang once.
                if opts.case_insensitive {
                    grep.arg("-i");
                }
                match opts.mode {
                    OutputMode::Content => {
                        // POSIX-portable spellings: BSD grep (macOS) has the
                        // short forms but not the GNU long ones.
                        if opts.before > 0 {
                            grep.arg("-B").arg(opts.before.to_string());
                        }
                        if opts.after > 0 {
                            grep.arg("-A").arg(opts.after.to_string());
                        }
                    }
                    OutputMode::FilesWithMatches => {
                        grep.arg("-l");
                    }
                    // `grep -c` counts matching LINES, not matches — it has no
                    // `--count-matches`. Documented here rather than papered
                    // over: the fallback's count can undercount a line with
                    // two hits, and inventing a second counting pass in Rust
                    // would make the two backends disagree in a different way.
                    OutputMode::Count => {
                        grep.arg("-c");
                    }
                }
                // `-e <pattern>` for the same leading-`-` safety as rg above.
                grep.arg("-e").arg(pattern).arg(&search_dir);
                crate::subprocess_env::scrub_sensitive_env(&mut grep);
                grep.stdout(std::process::Stdio::piped());
                grep.stderr(std::process::Stdio::piped());
                // Same cancellation backstop as the rg arm above.

                match crate::exec::run_captured(grep, FALLBACK_SEARCH_TIMEOUT_SECS).await {
                    crate::exec::Captured::TimedOut => ToolOutput::Error {
                        message: format!(
                            "grep timed out after {FALLBACK_SEARCH_TIMEOUT_SECS}s — narrow the \
                             search with a `path` or `glob` filter"
                        ),
                    },
                    crate::exec::Captured::Done(output) => {
                        // `grep` has no `--max-columns`, so the column cap is
                        // applied here; then the same byte-then-line order as
                        // the rg arm.
                        let text = crate::exec::truncate_middle(cap_columns(
                            &String::from_utf8_lossy(&output.stdout),
                        ));
                        // Recursive `grep -c` emits a row per file WALKED,
                        // including `path:0` for every file that did not
                        // match; rg's `--count-matches` emits only files that
                        // did. Dropping the zeros is what makes the two
                        // backends answer the same question — without it a
                        // count search in a large tree is mostly noise, and
                        // the `MAX_RESULTS` cap would be spent on zeros.
                        let text = if opts.mode == OutputMode::Count {
                            drop_zero_counts(&text)
                        } else {
                            text
                        };
                        if !text.is_empty() {
                            let lines: Vec<&str> = text.lines().take(MAX_RESULTS).collect();
                            let mut result = lines.join("\n");
                            // Say so when the cap bit, exactly as the rg arm
                            // does: a silently truncated match list reads to
                            // the agent as "there are no other call sites".
                            if lines.len() == MAX_RESULTS {
                                let unit = opts.mode.unit(opts.has_context());
                                result.push_str(&format!(
                                    "\n... (showing first {MAX_RESULTS} {unit})"
                                ));
                            }
                            if let Some(map) =
                                code_map_for(self.footer, show_tip, &search_dir, root, &lines)
                            {
                                result.push_str(&map);
                            }
                            return ToolOutput::Ok { content: result };
                        }
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if output.status.code() == Some(2) && is_pattern_error(&stderr) {
                            return ToolOutput::Error {
                                message: format!("grep error: {}", first_error_line(&stderr)),
                            };
                        }
                        no_matches(self.footer, show_tip, root)
                    }
                    crate::exec::Captured::Unavailable => ToolOutput::Error {
                        message: "grep failed: neither `rg` nor `grep` is available".into(),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exit-2 stderr is a pattern error only when the diagnostic follows the
    /// tool prefix — a keyword inside a warned-about *path* must not turn a
    /// zero-match walk into a failure.
    #[test]
    fn pattern_errors_are_distinguished_from_path_warnings() {
        assert!(is_pattern_error(
            "rg: regex parse error:\n    (foo\n    ^\nerror: unclosed group"
        ));
        assert!(is_pattern_error("grep: Invalid regular expression"));
        assert!(is_pattern_error("grep: Unmatched ( or \\("));
        assert!(!is_pattern_error(
            "rg: src/invalid/notes.txt: Permission denied (os error 13)"
        ));
        assert!(!is_pattern_error(
            "grep: ./error:pages/dump.log: Is a directory"
        ));
        assert!(!is_pattern_error(""));
    }

    /// A context line uses `-` where a match uses `:`, so the pre-#1260 path
    /// extraction (`split(':').next()`) handed the graph a whole context line
    /// — and since the search dir is absolute, `is_absolute()` agreed and the
    /// code-map footer silently emptied out exactly when context was on.
    #[test]
    fn a_context_line_yields_its_path_not_the_whole_line() {
        assert_eq!(
            path_of_result_line("/w/src/lib.rs:12:fn main() {"),
            Some("/w/src/lib.rs")
        );
        assert_eq!(
            path_of_result_line("/w/src/lib.rs-11-// preceding"),
            Some("/w/src/lib.rs")
        );
        // `--` separates non-contiguous context runs and names no file.
        assert_eq!(path_of_result_line("--"), None);
        // `-l` emits a bare path; `-c` emits `path:COUNT` (one separator, so
        // not a field break).
        assert_eq!(path_of_result_line("/w/src/lib.rs"), Some("/w/src/lib.rs"));
        assert_eq!(
            path_of_result_line("/w/src/lib.rs:7"),
            Some("/w/src/lib.rs")
        );
        // A `-N-` run inside the PATH must not beat the real `:` separator.
        assert_eq!(
            path_of_result_line("/w/src/x-12-y.rs:5:hit"),
            Some("/w/src/x-12-y.rs")
        );
    }

    /// Context multiplies the payload, so an unclamped value is a way to spend
    /// the whole line budget on one search.
    #[test]
    fn context_is_clamped_and_wrong_types_are_named_as_wrong_types() {
        let opts = SearchOptions::parse(&serde_json::json!({"context": 9_000})).unwrap();
        assert_eq!(opts.before, MAX_CONTEXT_LINES);
        assert_eq!(opts.after, MAX_CONTEXT_LINES);

        // Directional fields win over the symmetric shorthand.
        let opts =
            SearchOptions::parse(&serde_json::json!({"context": 3, "context_after": 1})).unwrap();
        assert_eq!((opts.before, opts.after), (3, 1));

        // Wrong-typed reports wrong-typed, not "missing" (#1267's defect).
        let err = SearchOptions::parse(&serde_json::json!({"context": "two"})).unwrap_err();
        assert!(err.contains("non-negative integer"), "got {err}");
    }

    /// Silently dropping a field teaches the model the field did nothing, so
    /// the next call repeats the mistake. Naming the conflict fixes it in one
    /// turn.
    #[test]
    fn context_in_a_non_content_mode_is_refused_rather_than_ignored() {
        let err = SearchOptions::parse(
            &serde_json::json!({"output_mode": "files_with_matches", "context": 2}),
        )
        .unwrap_err();
        assert!(err.contains("only meaningful"), "got {err}");

        let err = SearchOptions::parse(&serde_json::json!({"output_mode": "nope"})).unwrap_err();
        assert!(err.contains("output_mode"), "got {err}");

        // Context with no mode given is fine — content is the default.
        assert!(SearchOptions::parse(&serde_json::json!({"context": 2})).is_ok());
    }

    /// Recursive `grep -c` reports every file it WALKED, `path:0` included;
    /// rg's `--count-matches` reports only files that matched. Without this
    /// the fallback's result is mostly zeros and the cap is spent on them.
    #[test]
    fn the_fallback_count_drops_files_that_did_not_match() {
        let got = drop_zero_counts("/w/a.rs:3\n/w/b.rs:0\n/w/c.rs:1\n");
        assert_eq!(got, "/w/a.rs:3\n/w/c.rs:1");
        // A path containing `:` keeps its count read from the right.
        assert_eq!(drop_zero_counts("/w/od:d.rs:2"), "/w/od:d.rs:2");
        assert_eq!(drop_zero_counts("/w/od:d.rs:0"), "");
    }

    /// With context on, most returned lines did NOT match, so reporting them
    /// as "matches" misstates the search's own reach.
    #[test]
    fn the_truncation_note_names_lines_when_context_is_on() {
        assert_eq!(OutputMode::Content.unit(false), "matches");
        assert_eq!(OutputMode::Content.unit(true), "lines");
        assert_eq!(OutputMode::FilesWithMatches.unit(false), "files");
        assert_eq!(OutputMode::Count.unit(false), "files");
    }

    #[tokio::test]
    async fn context_lines_surround_the_hit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "line one\nline two\nNEEDLE here\nline four\nline five\n",
        )
        .unwrap();

        let out = Grep::bare()
            .execute(
                &serde_json::json!({"pattern": "NEEDLE", "context": 1}),
                dir.path(),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert!(content.contains("NEEDLE here"), "{content}");
        assert!(
            content.contains("line two") && content.contains("line four"),
            "context lines must surround the hit: {content}"
        );
        assert!(
            !content.contains("line one") && !content.contains("line five"),
            "context of 1 must not reach 2 lines out: {content}"
        );
    }

    #[tokio::test]
    async fn files_with_matches_returns_paths_and_count_returns_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "NEEDLE\nNEEDLE\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "nothing here\n").unwrap();

        let out = Grep::bare()
            .execute(
                &serde_json::json!({"pattern": "NEEDLE", "output_mode": "files_with_matches"}),
                dir.path(),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert!(content.contains("a.rs"), "{content}");
        assert!(
            !content.contains("b.rs"),
            "non-matching file must not appear: {content}"
        );
        assert!(
            !content.contains("NEEDLE"),
            "files_with_matches must not return line text: {content}"
        );

        let out = Grep::bare()
            .execute(
                &serde_json::json!({"pattern": "NEEDLE", "output_mode": "count"}),
                dir.path(),
            )
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert!(
            content.contains("a.rs:2"),
            "expected a count of 2: {content}"
        );
        assert!(
            !content.contains("b.rs"),
            "zero-count file must not appear: {content}"
        );
    }

    #[tokio::test]
    async fn case_insensitive_matches_regardless_of_case() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "NeEdLe\n").unwrap();

        let sensitive = Grep::bare()
            .execute(&serde_json::json!({"pattern": "needle"}), dir.path())
            .await;
        let ToolOutput::Ok { content } = sensitive else {
            panic!("expected Ok");
        };
        assert!(content.contains("(no matches)"), "{content}");

        let insensitive = Grep::bare()
            .execute(
                &serde_json::json!({"pattern": "needle", "case_insensitive": true}),
                dir.path(),
            )
            .await;
        let ToolOutput::Ok { content } = insensitive else {
            panic!("expected Ok");
        };
        assert!(content.contains("NeEdLe"), "{content}");
    }

    #[tokio::test]
    async fn finds_matching_lines() {
        // Isolated dir (auto-removed on drop) rather than the shared system
        // temp — the same reasoning as `no_matches_returns_ok_empty`.
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("notes.txt"),
            "hello world\nfoo bar\nhello again\n",
        )
        .await
        .unwrap();

        let result = Grep::default()
            .execute(
                &serde_json::json!({"pattern": "hello", "path": "notes.txt"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("hello world") || content.contains("hello"));
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
    }

    #[tokio::test]
    async fn no_matches_returns_ok_empty() {
        // An isolated, empty dir — NOT the shared `std::env::temp_dir()`. That
        // directory collects files from every concurrent test and every other
        // process on the machine; a leftover `stella_candidate_*` source copy
        // there even contains this very pattern, so scanning it found spurious
        // hits and the test failed nondeterministically.
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("clean.txt"), "nothing to see here\n")
            .await
            .unwrap();
        let result = Grep::default()
            .execute(
                &serde_json::json!({"pattern": "zzz_not_found_xyz"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("no matches"), "{content}")
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
    }

    /// A pattern that begins with `-` (the everyday `->`, `-n`, `--foo`)
    /// must be searched, not parsed by rg/grep as an option. Before `-e`
    /// this errored and was swallowed as "(no matches)".
    #[tokio::test]
    async fn a_leading_dash_pattern_is_searched_not_parsed_as_a_flag() {
        let dir = std::env::temp_dir().join(format!("stella_grep_dash_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("code.rs"), "fn f() -> i32 { 0 }\n")
            .await
            .unwrap();

        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "->"}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(
                content.contains("->"),
                "the arrow pattern should match the fixture, got: {content}"
            ),
            ToolOutput::Error { message } => {
                panic!("a `->` search should succeed, got error: {message}")
            }
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A search under a dot-prefixed directory used to answer "(no matches)"
    /// on every machine with `rg` installed, because `rg` skips hidden entries
    /// by default — and to answer correctly on every machine without it. An
    /// agent reads that silence as "the code isn't there".
    #[tokio::test]
    async fn a_dotted_directory_is_searched_and_git_internals_are_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".github/workflows")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("mkdir");
        std::fs::write(
            dir.path().join(".github/workflows/ci.yml"),
            "run: cargo NEEDLE_TOKEN\n",
        )
        .expect("write");
        std::fs::write(dir.path().join(".git/COMMIT_EDITMSG"), "NEEDLE_TOKEN\n").expect("write");

        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "NEEDLE_TOKEN"}), dir.path())
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("ci.yml"),
                    "a dotted directory must be searched: {content}"
                );
                assert!(
                    !content.contains("COMMIT_EDITMSG"),
                    "`.git` must stay out of the results: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }

    /// Short lines must survive the column cap byte-for-byte — the caps are
    /// invisible for every ordinary search.
    #[test]
    fn the_column_cap_leaves_ordinary_lines_untouched() {
        let ordinary = "src/a.rs:1:fn main() {}\nsrc/b.rs:9:    let x = 1;";
        assert_eq!(cap_columns(ordinary), ordinary);
        assert_eq!(cap_columns(""), "");
    }

    #[test]
    fn the_column_cap_clips_a_long_line_loudly_on_a_char_boundary() {
        // Multi-byte chars straddling the cut: byte slicing would panic.
        let long = format!("f.rs:1:{}", "é".repeat(1_000));
        let capped = cap_columns(&long);
        assert!(capped.len() < long.len(), "clipped");
        assert!(capped.contains("bytes elided"), "loudly: {capped}");
    }

    /// One match inside a minified bundle used to be forwarded to the model
    /// in full — a single "line" of megabytes. Whichever backend runs (rg's
    /// `--max-columns-preview` or the fallback's own cap), the result must
    /// be bounded.
    #[tokio::test]
    async fn a_match_in_a_minified_line_is_not_emitted_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blob = "x".repeat(200_000);
        std::fs::write(
            dir.path().join("bundle.min.js"),
            format!("{blob}NEEDLE{blob}\n"),
        )
        .expect("write");

        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "NEEDLE"}), dir.path())
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(
                    content.len() < 32_000,
                    "a 400 KB match line must not reach the model whole (got {} bytes)",
                    content.len()
                );
            }
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }

    /// An invalid regex must surface as an error, not be swallowed as
    /// "(no matches)" — otherwise the agent silently believes its search
    /// found nothing when the query itself was broken.
    #[tokio::test]
    async fn an_invalid_regex_surfaces_an_error() {
        let dir = std::env::temp_dir().join(format!("stella_grep_badre_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("f.txt"), "hello\n")
            .await
            .unwrap();

        // Unclosed character class — invalid for both rg and grep.
        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "[unclosed"}), &dir)
            .await;
        assert!(
            matches!(result, ToolOutput::Error { .. }),
            "a broken regex must report an error, got: {result:?}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A workspace with one indexed source file, exactly what `stella init`
    /// leaves behind.
    fn indexed_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\n",
        )
        .expect("write source");
        let db = crate::graph::graph_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(dir.path(), &db).expect("open graph");
        graph.index_all().expect("index");
        graph.shutdown();
        dir
    }

    #[tokio::test]
    async fn matches_carry_a_code_map_footer_when_an_index_exists() {
        let dir = indexed_workspace();
        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "greet"}), dir.path())
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("lib.rs"), "the raw match stays: {content}");
                assert!(content.contains("code map"), "footer attached: {content}");
                assert!(
                    content.contains("function greet:1"),
                    "maps the matched file's symbols: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }

    /// rg/grep omit the filename when handed a single file, so the footer
    /// must come from the search target itself, not the match prefix.
    #[tokio::test]
    async fn a_single_file_search_still_maps_the_target_file() {
        let dir = indexed_workspace();
        let result = Grep::default()
            .execute(
                &serde_json::json!({"pattern": "greet", "path": "lib.rs"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("function greet:1"),
                    "single-file searches map the target: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }

    /// A symbol-shaped pattern carries the `graph_query` pointer; a free-text
    /// scan gets the map bare — the nudge lands only where the graph wins.
    #[tokio::test]
    async fn the_graph_tip_rides_symbol_shaped_searches_only() {
        let dir = indexed_workspace();
        let symbol = Grep::default()
            .execute(&serde_json::json!({"pattern": "greet"}), dir.path())
            .await;
        match symbol {
            ToolOutput::Ok { content } => assert!(
                content.contains("graph_query"),
                "a bare-identifier search nudges: {content}"
            ),
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }

        // A regex text pattern that still matches the file but isn't a symbol
        // hunt: map yes, tip no.
        let text = Grep::default()
            .execute(&serde_json::json!({"pattern": "gr.*t"}), dir.path())
            .await;
        match text {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("code map"),
                    "map still attaches: {content}"
                );
                assert!(
                    !content.contains("graph_query"),
                    "no nudge on a text scan: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }

    /// #896: a symbol hunt that matched NOTHING is the moment the steer is
    /// worth most — the agent has just been told the text is absent and the
    /// graph is the only surface that can still answer (a different spelling,
    /// or the definition the text scan missed). The answer used to be a bare
    /// "(no matches)" with no pointer at all.
    #[tokio::test]
    async fn a_symbol_hunt_with_no_matches_still_points_at_the_graph() {
        let dir = indexed_workspace();
        let result = Grep::default()
            .execute(
                &serde_json::json!({"pattern": "NoSuchSymbolHere"}),
                dir.path(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("no matches"), "{content}");
                assert!(
                    content.contains("graph_query"),
                    "an empty symbol hunt steers at the graph: {content}"
                );
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }

        // A free-text scan that finds nothing stays bare — the nudge must not
        // decay into a footer on every empty search.
        let text = Grep::default()
            .execute(
                &serde_json::json!({"pattern": "no.*such.*thing"}),
                dir.path(),
            )
            .await;
        match text {
            ToolOutput::Ok { content } => assert!(
                !content.contains("graph_query"),
                "no nudge on an empty text scan: {content}"
            ),
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
    }

    #[tokio::test]
    async fn no_index_means_no_code_map_footer() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("lib.rs"), "pub fn greet() {}\n").expect("write");
        let result = Grep::default()
            .execute(&serde_json::json!({"pattern": "greet"}), dir.path())
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(
                !content.contains("code map"),
                "no index → the result is exactly the matches: {content}"
            ),
            ToolOutput::Error { message } => panic!("expected matches, got: {message}"),
        }
    }
}
