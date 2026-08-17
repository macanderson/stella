//! `bash` — run a shell command in the workspace root with a timeout.
//! Process-group based kill so children don't outlive the timeout — or a
//! cancelled turn: the driving future being dropped arms the same group kill
//! (`crate::exec::GroupKillGuard`).
//!
//! **Registered by default, switchable off.** This tool ships on, like every
//! other built-in; `"tools": {"bash": "off"}` in `settings.json` withholds it
//! (see [`crate::policy::ToolPolicy`], enforced above the whole session tool
//! stack rather than at construction). It used to be the reverse — opt-in
//! through a `RegistryOptions` boolean — which covered built-ins only and
//! meant most operators simply never found the switch. Prefer the structured
//! executors anyway — `run_tests`, `build_project`, `run_lint`, `format_code`,
//! `run_script`, the process group, and the `repo_*` tools — which spawn
//! enumerable argv and never interpret a shell string.
//!
//! Switching it off bounds the *general-purpose* shell, not every path to one.
//! `build_project` and `run_tests` accept a `command` override,
//! `verify_done` a `test_cmd`, and `run_script` composes from the scripts
//! index — all four reach `bash -c` through `crate::exec::run`. So
//! `"bash": "off"` removes the shell TOOL, not the shell CAPABILITY; the fence
//! that covers every one of them uniformly is the registry's `command.started`
//! policy chain (see `ToolRegistry::command_line_for`), which is why that chain
//! enumerates them all — and, since #615, the text written into an already
//! running interpreter as well. An operator who needs shell execution actually
//! contained wants that chain plus a boundary the whole process sits inside.
//!
//! # This tool carries no confinement of its own, and says so
//!
//! **There is no session-wide OS sandbox here, and no text audit of the
//! command either.** Both existed once and both were tied to a caller that no
//! longer does: `STELLA_BASH_SANDBOX`, the opt-in Seatbelt/`bwrap` wrapper,
//! was removed in #1300 for claiming a session-wide bound it never had (it
//! wrapped this one tool while every other spawn path ran around it); the
//! `confine`/`contain` pair that replaced it audited the command text and put
//! an OS write ban on a *graded tree*, armed only inside an isolated
//! best-of-N candidate workspace — a registry configuration this crate no
//! longer builds.
//!
//! Restoring either as it stood would be unwired code with no caller, so this
//! module ships without them and names the gap rather than implying a
//! boundary. Session isolation belongs to the container the whole Stella
//! process runs in (`docs/spec/remote-sandboxes.md` §2), and what bounds an
//! individual call is the registry's `tool.call.requested` policy chain and
//! the operator's `"tools": {"bash": "off"}` switch. Re-arming a real
//! per-command boundary is tracked work, not a silence.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use crate::registry::Tool;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Byte cap on one `bash` result before head+tail elision
/// ([`crate::exec::truncate_middle_capped`]). [`crate::custom`] aliases this
/// constant, so the two shell-shaped surfaces cannot drift apart (#1889).
///
/// 64 KB ≈ 18k estimated tokens ≈ 12% of the 150k compaction budget — the
/// point #1842 ratified for `read_file`, for the same reason: one large
/// result does not trigger the compaction that would reclaim it
/// (`compact_measured` returns early when the rest of the transcript is
/// small), and the 8-step retention horizon then keeps it verbatim, so its
/// real cost is eight times its size. The previous 100 KB (#616) put ~19% of
/// the budget in a single result, ~224k input tokens over the horizon.
///
/// Still 2.2x `exec::MAX_OUTPUT_BYTES` (30k), preserving #616's ratio
/// argument: the shell is the agent's primary sensory channel — one `bash`
/// call renders a whole build or test run whose first error and final summary
/// sit far apart — while the `exec` budget bounds each incremental
/// `read_output` page of a *managed* process the agent polls repeatedly.
/// Aligning them would either starve the shell or inflate every page read.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// grep-family commands whose first positional arg is a search pattern.
const GREP_CMDS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ripgrep", "ag"];

/// A quote-aware word split — enough to pull a `cd` target or a `grep`
/// pattern out of the common command shapes, returning each word already
/// unquoted. NOT a shell parser: it respects `'…'` and `"…"` (so a pattern or
/// path with spaces stays one word) and preserves backslash escapes like
/// `\|` (so an alternation survives into [`is_symbol_shaped`](crate::code_map::is_symbol_shaped)); unquoted
/// operators (`&&`, `||`, `|`, `;`, `&`) come back as their own words to
/// bound a scan — including when attached to a word, so `cd /app; ls` yields
/// the target `/app`, not the unresolvable `/app;` a paid bench trial saw
/// warned about as an escape from its own session root.
fn shell_words(command: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_word = false;
    let (mut in_single, mut in_double) = (false, false);
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                has_word = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_word = true;
            }
            c @ (';' | '&' | '|') if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                let mut op = String::from(c);
                // `&&` / `||` are one operator; `;;` never appears in the
                // shapes this scans, so `;` stays single.
                if c != ';' && chars.peek() == Some(&c) {
                    chars.next();
                    op.push(c);
                }
                words.push(op);
            }
            '\\' if !in_single => {
                // Keep the escape literal (covers `\|`, `\"`, …); we don't
                // interpret it, just preserve it for the symbol test.
                cur.push('\\');
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                has_word = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            c => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words
}

/// If the command `cd`s somewhere outside the workspace `root`, return that
/// target — every other tool is confined to `root`, so an edit under a drifted
/// tree is invisible to the file tools, the turn's diff, and verification (and
/// ungraphed besides, the cross-tree gap the telemetry showed). Targets we
/// can't resolve (`$VAR`, `~`, `-`, a flag) are skipped: better a missed note
/// than a wrong one.
fn cd_escape_target(command: &str, root: &Path) -> Option<String> {
    let words = shell_words(command);
    for pair in words.windows(2) {
        if pair[0] != "cd" {
            continue;
        }
        let target = pair[1].as_str();
        // `-` (cd to previous dir) is caught by the `-` prefix below. An
        // operator word means the `cd` had no target at all (`cd && make`):
        // that goes to `$HOME`, which we skip for the same reason as `~`.
        if target.is_empty()
            || target.starts_with('$')
            || target.starts_with('~')
            || target.starts_with('-')
            || matches!(target, ";" | "&" | "&&" | "|" | "||")
        {
            continue;
        }
        if crate::resolve_within_root(root, target).is_none() {
            return Some(target.to_string());
        }
    }
    None
}

/// Does the command run a grep-family search whose pattern is symbol-shaped —
/// the `grep -rn "struct X"` that graph_query answers better? The dominant
/// path in the telemetry (symbol searches ran through bash, not the native
/// grep tool), so the same nudge has to reach here. First positional after a
/// grep word is the pattern; flags are skipped, a pipeline boundary ends the
/// scan.
fn bash_grep_is_symbol_shaped(command: &str) -> bool {
    let words = shell_words(command);
    for (i, w) in words.iter().enumerate() {
        if !GREP_CMDS.contains(&w.as_str()) {
            continue;
        }
        for next in &words[i + 1..] {
            if matches!(next.as_str(), "|" | "||" | "&&" | ";") {
                break;
            }
            if next.starts_with('-') {
                continue; // a flag, not the pattern
            }
            if is_symbol_shaped(next) {
                return true;
            }
            break; // first positional was the pattern; it wasn't symbol-shaped
        }
    }
    false
}

/// Declaration keywords a symbol hunt commonly leads with, across the
/// languages this repository's users actually search.
const DECL_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "function",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "type",
    "const",
    "static",
    "mod",
    "module",
    "pub",
    "public",
    "private",
    "protected",
    "use",
    "import",
    "let",
    "var",
    "val",
    "namespace",
];

/// Does this pattern look like the agent hunting for where a symbol is defined
/// or used — the case [`crate::search`] serves better than a text scan?
/// Applied to every `|`-alternation branch (so a multi-symbol
/// `struct A|struct B` still counts), each branch must be one of two shapes:
///   * a lone identifier or `::`/`.`-path — `ReadOnlyTools`, `stella::graph::Foo`
///   * a declaration-keyword-led identifier — `struct Foo`, `pub fn bar`
///
/// A single leading `^` / trailing `$` anchor is tolerated (the everyday
/// `^pub fn` definition search); any other regex machinery — quantifiers,
/// char classes, wildcards, embedded metacharacters — means a genuine text
/// pattern, grep's own job, and does NOT trip the nudge. Deliberately errs
/// toward missing over false-firing: a stray tip line is cheap, but nudging on
/// a real text scan is noise on every call.
fn is_symbol_shaped(pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    // `\|` (escaped, from a shell-quoted rg) and `|` (raw regex) both mean
    // alternation here; normalize, then every branch must be a symbol.
    let normalized = pattern.replace("\\|", "|");
    let mut branches = 0usize;
    for branch in normalized.split('|') {
        branches += 1;
        if !branch_is_symbol(branch) {
            return false;
        }
    }
    branches > 0
}

/// One alternation branch: an optional run of declaration keywords followed
/// by a single identifier/path. `^`/`$` anchors are stripped first.
fn branch_is_symbol(branch: &str) -> bool {
    let branch = branch
        .trim()
        .trim_start_matches('^')
        .trim_end_matches('$')
        .trim();
    let tokens: Vec<&str> = branch.split_whitespace().collect();
    let Some((ident, keywords)) = tokens.split_last() else {
        return false;
    };
    keywords
        .iter()
        .all(|k| DECL_KEYWORDS.contains(&k.to_ascii_lowercase().as_str()))
        && is_identifier_or_path(ident)
}

/// A bare identifier or a `::`/`.`-separated path of them — how a symbol name
/// is written. Each segment starts alphabetic/underscore and is otherwise
/// alphanumeric/underscore; the whole must contain at least one letter, so a
/// bare number never counts as a symbol.
fn is_identifier_or_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split("::").flat_map(|p| p.split('.')).all(|seg| {
        let mut chars = seg.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }) && s.chars().any(|c| c.is_ascii_alphabetic())
}

/// The tip a symbol-shaped grep earns: [`crate::search`] answers the same
/// question by meaning and returns the symbol's neighborhood already attached,
/// so the usual `grep -rn` → `read_file` → `grep` again round trip collapses
/// into one call.
const SEARCH_TIP: &str = "note: that pattern looks like a hunt for where a symbol is defined or \
                          used. `search` answers those directly — it matches by meaning as well \
                          as text and returns each file with its symbols, callers and imports \
                          attached, so one call usually replaces several grep/read_file round \
                          trips. Keep using `grep` when you need every occurrence of one exact \
                          literal string.";

/// The advisory footer for a bash result: a cross-root `cd` warning takes
/// precedence, otherwise a symbol-shaped grep is pointed at [`crate::search`].
///
/// Neither note is conditioned on a code-graph index existing. The grep tip
/// used to be, because it advertised a tool (`graph_query`) that genuinely was
/// not there without one; `search` always answers — its bottom rung is an
/// index-free file scan — so the advice is never about a tool that is missing.
/// The drift warning never was gated, and must not become so: gating it on the
/// index meant the one case that most needs it, a freshly-created tree with
/// nothing indexed, was the one case that stayed silent.
fn drift_advisory(command: &str, root: &Path) -> Option<String> {
    if let Some(target) = cd_escape_target(command, root) {
        // The remedy has to be one the *agent* can perform. "Re-root the
        // session" is the user's move, not the model's, and offering it as
        // the answer is how `build-cython-ext` step 5 concluded that the
        // graded tree was its own workspace under another name and ran
        // `rm -rf /app/pyknotid` — the exact two grader tests it then failed.
        return Some(format!(
            "\n\nnote: this cd'd to `{target}`, outside the session root `{}`. Only \
             work under the session root is collected when the turn finishes — every \
             other tool is confined to it, so edits under `{target}` are invisible to \
             the file tools, to this turn's diff, and to verification. Copy what you \
             need under the session root and work there; only the user can re-root the \
             session on another tree.",
            root.display()
        ));
    }
    if bash_grep_is_symbol_shaped(command) {
        return Some(format!("\n\n{SEARCH_TIP}"));
    }
    None
}

/// A bare `sleep` blocking the whole trial budget goes undetected today:
/// loop detection sees polls (`read_output`) between the sleeps so the
/// interleaved-repeat rung reads the window as progressing, and the budget
/// guard is spend-based, so idling costs $0. #2022's first step is honest
/// visibility, not a refusal — a static text-shape check on the command, not
/// a measured elapsed time, so it stays deterministic for the loop detector
/// (never embed a timing here; see `stella-tool-timings-must-not-ride-tooloutput`).
///
/// Only a *bare* sleep is worth naming: `sleep 2 && curl` retry backoffs are
/// ordinary and must stay unflagged, so this requires every segment of the
/// command to be `sleep N` or an inert no-op (`echo`, `printf`, `true`) —
/// anything else (a real command sharing the line) disqualifies the whole
/// command, matching the observed pathological shape (`sleep 300; echo
/// done`) rather than a compound one that happens to contain a sleep.
const SLEEP_ADVISORY_THRESHOLD_SECS: u64 = 30;

/// The accumulated seconds a *bare* sleep command blocks for, or `None` if
/// any segment does real work beyond sleeping and a harmless no-op.
fn bare_sleep_seconds(command: &str) -> Option<u64> {
    let words = shell_words(command);
    let mut segments: Vec<&[String]> = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        if matches!(w.as_str(), ";" | "&" | "&&" | "|" | "||") {
            segments.push(&words[start..i]);
            start = i + 1;
        }
    }
    segments.push(&words[start..]);

    let mut total_secs = 0u64;
    let mut saw_sleep = false;
    for segment in &segments {
        match segment {
            [] => {}
            [cmd, arg] if cmd == "sleep" => {
                let secs = arg.parse::<f64>().ok()?;
                total_secs += secs.round() as u64;
                saw_sleep = true;
            }
            [cmd, ..] if matches!(cmd.as_str(), "echo" | "printf" | "true") => {}
            _ => return None,
        }
    }
    saw_sleep.then_some(total_secs)
}

/// A footer naming a bare `sleep` that crossed the advisory threshold, and
/// the instrument (`read_output`/`wait_for`) that returns as soon as there is
/// something to see instead of blocking the whole interval.
fn sleep_advisory(command: &str) -> Option<String> {
    let secs = bare_sleep_seconds(command)?;
    if secs < SLEEP_ADVISORY_THRESHOLD_SECS {
        return None;
    }
    Some(format!(
        "\n\nnote: this call blocked for {secs}s inside a bare `sleep` with no other work in \
         it. If you are waiting on a background process, poll with `read_output`/`wait_for` \
         instead — they return as soon as there is something to see, so a short poll costs less \
         of the trial's budget than sleeping through the whole interval."
    ))
}

/// `bash`: one shell command in the workspace root, with a timeout backstop
/// and a process-group kill that reaches the children it spawned.
pub struct Bash {
    /// The session scratch directory, exported to the command as
    /// `STELLA_SCRATCH` so a working file has somewhere to live that is
    /// neither the workspace diff nor `/tmp`.
    scratch: Option<std::path::PathBuf>,
}

impl Bash {
    /// The shell for a session whose scratch plane is `scratch` (`None` when
    /// the plane failed to initialize — the variable is then simply absent
    /// rather than set to an empty path).
    #[must_use]
    pub fn new(scratch: Option<std::path::PathBuf>) -> Self {
        Self { scratch }
    }
}

#[async_trait]
impl Tool for Bash {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description: "Run a shell command in the workspace root. Returns stdout+stderr with a timeout backstop.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600; 0 = default)" },
                    "trace": { "type": "boolean", "description": "Echo each executed line (set -x) as an execution trace" }
                },
                "required": ["command"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let root = ctx.root();
        let command = match crate::input::required_str(input, "command") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let timeout_secs = crate::exec::timeout_from(input, DEFAULT_TIMEOUT_SECS);
        // trace: true prefixes `set -x` so every executed line echoes to
        // stderr — an execution trace a verifier can demand as evidence.
        // A wrong-typed `trace` is refused, never silently read as false:
        // the caller believes it armed the trace (#3144).
        let trace = match crate::input::optional_bool(input, "trace") {
            Ok(trace) => trace.unwrap_or(false),
            Err(err) => {
                return ToolOutput::from(err);
            }
        };
        let traced;
        let command = if trace {
            traced = format!("set -x\n{command}");
            traced.as_str()
        } else {
            command
        };

        let mut cmd = Command::new("bash");
        cmd.args(["-c", command]);
        cmd.current_dir(root);
        // Full spawn policy, not just the credential scrub: a hook-exported
        // GIT_DIR or a forced-color override must not reach the shell either
        // (same families `exec::drive` removes for every other runner).
        crate::subprocess_env::scrub_spawn_env(&mut cmd);
        // Inject the session scratch directory path AFTER the scrub, so the
        // scrub cannot remove it.
        crate::subprocess_env::inject_scratch_env(&mut cmd, self.scratch.as_deref());
        // No stdin. An inherited stdin is the TUI's terminal: a command that
        // reads it (`cat`, an interactive prompt, a confirmation `read`)
        // silently steals the user's keystrokes and then blocks until the
        // timeout. `/dev/null` makes the read an immediate EOF, which is
        // what every non-interactive runner already expects.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Reap the direct child if the driving future is dropped (tokio keeps
        // it running by default). A backstop only — it does NOT reach the
        // grandchildren the shell spawned, which is what the unix
        // `GroupKillGuard` below is for; this is what covers the non-unix
        // build, where that guard does not exist.
        cmd.kill_on_drop(true);
        // New process group so we can kill the whole tree on timeout.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::error(format!("failed to spawn: {e}"));
            }
        };

        // Capture pid before the capped wait takes ownership.
        #[cfg(unix)]
        let pid = child.id().unwrap_or(0) as i32;
        // Cancellation backstop: a dropped future (Esc, the engine's tool
        // timeout, a fleet stop) must not leave the setsid'd group running.
        #[cfg(unix)]
        let mut guard = crate::exec::GroupKillGuard::arm(pid);

        let timeout = Duration::from_secs(timeout_secs);
        // Capped capture, not `wait_with_output`: the command text is the
        // model's, and the middle-out cut below only ever ran once the whole
        // stream was already resident — so `yes` or `cat /dev/urandom | base64`
        // grew Stella's RSS until the OOM killer beat the timeout to it. See
        // `crate::exec::MAX_CAPTURE_BYTES`.
        let output = match tokio::time::timeout(
            timeout,
            crate::exec::wait_with_capped_output(child, crate::exec::MAX_CAPTURE_BYTES),
        )
        .await
        {
            Ok(Ok(output)) => {
                #[cfg(unix)]
                guard.disarm();
                output
            }
            // Wait failure leaves the child's state unknown — the still-armed
            // guard kills the group on return rather than leak it.
            Ok(Err(e)) => {
                return ToolOutput::error(format!("command failed: {e}"));
            }
            Err(_) => {
                // Timeout — kill the process group.
                #[cfg(unix)]
                guard.kill_now();
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::Timeout,
                    format!("command timed out after {timeout_secs}s"),
                );
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status.code().unwrap_or(-1);

        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("[stderr]\n");
            combined.push_str(&stderr);
        }
        combined.push_str(&format!("\n[exit code: {status}]"));

        // Over the cap: keep head and tail, elide the middle loudly — the
        // tail carries the part that usually matters (test summary, last
        // error), and the marker names what fell out. One shared spelling
        // for every shell-shaped surface (`crate::exec::truncate_middle_capped`).
        if combined.len() > MAX_OUTPUT_BYTES {
            combined = crate::exec::truncate_middle_capped(&combined, MAX_OUTPUT_BYTES);
        }

        // Append after truncation so the steer is never the part that gets
        // cut: a cross-root `cd` warning or a symbol-shaped-grep graph_query
        // nudge, when an index exists.
        if let Some(note) = drift_advisory(command, root) {
            combined.push_str(&note);
        }
        if let Some(note) = sleep_advisory(command) {
            combined.push_str(&note);
        }

        if output.status.success() {
            ToolOutput::ok(combined)
        } else {
            ToolOutput::error(combined)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_echo_command() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(&serde_json::json!({"command": "echo hello_stella"}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("hello_stella")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    /// The #3144 witness: a wrong-typed `trace` is refused, never silently
    /// read as `false`. On main, `{"trace": "yes"}` ran the command untraced
    /// — the caller believed it armed the trace it asked for as evidence.
    #[tokio::test]
    async fn a_mistyped_trace_is_refused_not_silently_untraced() {
        let dir = tempfile::tempdir().unwrap();
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "echo hi > probe.txt", "trace": "yes"}),
                dir.path(),
            )
            .await;
        let ToolOutput::Error { message, .. } = out else {
            panic!("a mistyped trace must be an error, got: {out:?}");
        };
        assert_eq!(message, "field `trace` must be a boolean, got string");
        assert!(
            !dir.path().join("probe.txt").exists(),
            "the command must not run on refused input"
        );
    }

    #[tokio::test]
    async fn bash_tool_scrubs_inherited_credentials() {
        let _fixture = crate::subprocess_env::test_support::InheritedCredentialFixture::install();
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({
                    "command": crate::subprocess_env::test_support::PROBE_COMMAND
                }),
                &std::env::temp_dir(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                let output = content.lines().next().unwrap_or_default();
                crate::subprocess_env::test_support::assert_scrubbed(output);
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    /// The bash tool used to apply only the credential scrub, letting a
    /// hook-exported GIT_DIR retarget every git it ran and a forced-color
    /// override wrap parsed output in ANSI escapes.
    #[tokio::test]
    async fn bash_tool_scrubs_git_repo_and_forced_color_env() {
        let _fixture = crate::subprocess_env::test_support::SpawnHygieneFixture::install();
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({
                    "command": crate::subprocess_env::test_support::SPAWN_HYGIENE_PROBE_COMMAND
                }),
                &std::env::temp_dir(),
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                let output = content.lines().next().unwrap_or_default();
                crate::subprocess_env::test_support::assert_spawn_hygiene_scrubbed(output);
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    /// Witness for #2666: a command that backgrounds a child (`sleep 5 &`,
    /// same shape as `nohup server &`) used to make `bash` hang until the
    /// FULL `timeout_secs` — the backgrounded child still holds this call's
    /// copy of the pipe open, so `wait_with_capped_output` waited for a true
    /// EOF that could not arrive until the child itself exited. The command
    /// the model actually asked for (`echo done_immediately`) finishes in
    /// milliseconds; the tool call must return in roughly that time, not in
    /// however long the backgrounded child happens to run.
    #[tokio::test]
    async fn does_not_block_on_a_backgrounded_child_holding_the_pipe_open() {
        let dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({
                    "command": "sleep 5 & echo done_immediately",
                    "timeout_secs": 10
                }),
                &dir,
            )
            .await;
        let elapsed = started.elapsed();
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("done_immediately"), "{content}")
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "bash blocked on a backgrounded child holding the pipe open: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn captures_stderr() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(&serde_json::json!({"command": "echo err >&2"}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("err")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(&serde_json::json!({"command": "exit 42"}), &dir)
            .await;
        assert!(result.is_error());
        if let ToolOutput::Error { message, .. } = result {
            assert!(message.contains("42"))
        }
    }

    #[tokio::test]
    async fn timeout_kills_command() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "sleep 30", "timeout_secs": 1}),
                &dir,
            )
            .await;
        assert!(result.is_error());
        if let ToolOutput::Error { message, .. } = result {
            assert!(message.contains("timed out"))
        }
    }

    /// Dropping the future mid-wait (a cancelled turn) must kill the whole
    /// process group — the `crate::exec::GroupKillGuard` backstop. Without
    /// it, Esc during a long `bash` call left the command running and
    /// mutating the tree, `setsid`'d beyond the reach of anything else.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_bash_call_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        // Record the *grandchild*'s pid: when the group dies, the orphaned
        // sleep is reaped by init, so a surviving pid means a real leak.
        // `kill_on_drop` alone would not reach it — only the group kill does.
        let command = format!("sleep 30 & echo $! > {} && wait", pidfile.display());
        let root = dir.path().to_path_buf();
        let handle = tokio::spawn(async move {
            Bash::new(None)
                .execute(
                    &serde_json::json!({"command": command, "timeout_secs": 60}),
                    &root,
                )
                .await
        });
        let mut pid = None;
        for _ in 0..250 {
            if let Some(p) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                pid = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = pid.expect("the child never started");
        handle.abort();
        let _ = handle.await;
        let mut dead = false;
        for _ in 0..250 {
            if unsafe { libc::kill(pid, 0) } == -1 {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "cancelled bash call left subprocess {pid} running");
    }

    #[tokio::test]
    async fn truncates_multibyte_output_without_panicking() {
        let dir = std::env::temp_dir();
        // Emit well over MAX_OUTPUT_BYTES of a 3-byte UTF-8 char, with no
        // newlines, so the middle cut lands inside a char. A raw byte slice
        // at that offset would panic; the boundary-safe path must not.
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "yes '€' | tr -d '\\n' | head -c 200000"}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => {
                assert!(content.contains("truncated"), "expected truncation marker");
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    /// Witness for #1889: output just over the cap keeps BOTH its first and
    /// last lines around an elision marker that names the cap. Sized from the
    /// constant — the filler alone fills the cap, so the sentinels push it
    /// over by a hair — which makes the test fail under any larger cap (the
    /// old 100 KB left this size untouched) and under any head-only cut.
    #[tokio::test]
    async fn over_cap_output_keeps_first_and_last_lines_with_a_named_elision() {
        let dir = std::env::temp_dir();
        let command = format!(
            "printf 'FIRST_SENTINEL_LINE\\n'; \
             head -c {MAX_OUTPUT_BYTES} /dev/zero | tr '\\0' 'x'; \
             printf '\\nLAST_SENTINEL_LINE\\n'"
        );
        let result = Bash::new(None)
            .execute(&serde_json::json!({ "command": command }), &dir)
            .await;
        let content = match result {
            ToolOutput::Ok { content } => content,
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        };
        assert!(
            content.contains("FIRST_SENTINEL_LINE"),
            "the head survives elision"
        );
        assert!(
            content.contains("LAST_SENTINEL_LINE"),
            "the tail survives elision"
        );
        assert!(
            content.contains(&format!("the {MAX_OUTPUT_BYTES}-byte cap")),
            "the marker names the cap it enforced: {}",
            &content[..200]
        );
        assert!(
            content.contains("bytes truncated"),
            "the marker names the elided byte count"
        );
        assert!(
            content.len() <= MAX_OUTPUT_BYTES + 256,
            "bounded by the cap plus the marker (got {} bytes)",
            content.len()
        );
    }

    #[tokio::test]
    async fn runs_in_workspace_root() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(&serde_json::json!({"command": "pwd"}), &dir)
            .await;
        match result {
            ToolOutput::Ok { content } => {
                // macOS temp_dir is a symlink; compare canonicalized paths.
                let pwd = content.lines().next().unwrap_or("").trim();
                let canonical_pwd = std::fs::canonicalize(pwd).unwrap_or_default();
                let canonical_dir = std::fs::canonicalize(&dir).unwrap_or_default();
                assert_eq!(
                    canonical_pwd,
                    canonical_dir,
                    "pwd `{pwd}` should resolve to workspace root `{}`",
                    canonical_dir.display()
                );
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    /// An indexed workspace — a source file plus a built `.stella/private/codegraph.db`,
    /// exactly what `stella init` leaves so `graph_available` is true.
    fn indexed_tempdir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub struct Greeter;\npub fn greet() {}\n",
        )
        .expect("write");
        let db = crate::graph::graph_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
        let graph = stella_graph::CodeGraph::open(dir.path(), &db).expect("open");
        graph.index_all().expect("index");
        graph.shutdown();
        dir
    }

    fn text_of(out: ToolOutput) -> String {
        match out {
            ToolOutput::Ok { content } => content,
            ToolOutput::Error { message, .. } => message,
        }
    }

    #[test]
    fn cd_escape_target_flags_out_of_root_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        // In-root cd — no drift.
        assert_eq!(cd_escape_target("cd sub && ls", dir.path()), None);
        assert_eq!(cd_escape_target("cd . && cargo build", dir.path()), None);
        // Out-of-root — the target is returned.
        assert_eq!(
            cd_escape_target("cd / && grep -rn x .", dir.path()).as_deref(),
            Some("/")
        );
        assert!(cd_escape_target("cd ../.. && ls", dir.path()).is_some());
        // Unresolvable / no cd — skipped.
        assert_eq!(cd_escape_target("cd $HOME && ls", dir.path()), None);
        assert_eq!(cd_escape_target("cd ~/foo && ls", dir.path()), None);
        assert_eq!(cd_escape_target("grep -rn x .", dir.path()), None);
    }

    #[test]
    fn shell_words_splits_operators_attached_to_a_word() {
        assert_eq!(shell_words("cd /app; ls"), ["cd", "/app", ";", "ls"]);
        assert_eq!(shell_words("a&&b"), ["a", "&&", "b"]);
        assert_eq!(
            shell_words("a || b|c & d"),
            ["a", "||", "b", "|", "c", "&", "d"]
        );
        // Quoted and escaped operators stay inside the word.
        assert_eq!(shell_words("echo 'a;b'"), ["echo", "a;b"]);
        assert_eq!(shell_words(r"grep x\|y ."), ["grep", r"x\|y", "."]);
    }

    /// The false positive a paid bench trial hit: `cd /app; …` with session
    /// root `/app` drew the "work here is invisible to the diff and to
    /// verification" warning, because the target parsed as the unresolvable
    /// `/app;`.
    #[test]
    fn an_attached_separator_does_not_fake_a_cd_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        // In-root cd with the separator attached — no drift, no warning.
        assert_eq!(cd_escape_target("cd sub; ls", &root), None);
        assert_eq!(
            cd_escape_target(&format!("cd {}; ls", root.display()), &root),
            None
        );
        // A real escape still warns, and names the clean target.
        assert_eq!(cd_escape_target("cd /; ls", &root).as_deref(), Some("/"));
        // A bare `cd` before an operator goes to `$HOME` — unresolvable
        // here, so it is skipped rather than misnamed.
        assert_eq!(cd_escape_target("cd && ls", &root), None);
    }

    #[test]
    fn bash_grep_symbol_detection() {
        assert!(bash_grep_is_symbol_shaped(
            r#"grep -rn "struct DeckProviderResolver" stella-tools/"#
        ));
        assert!(bash_grep_is_symbol_shaped("grep -n ReadOnlyTools src/"));
        assert!(bash_grep_is_symbol_shaped(r#"rg -e "pub fn resolve" ."#));
        assert!(bash_grep_is_symbol_shaped(
            r#"grep -rn "pub mod ports\|pub use ports" src/"#
        ));
        // free-text / non-symbol patterns — no nudge
        assert!(!bash_grep_is_symbol_shaped(r#"grep -rn "unwrap()" src/"#));
        assert!(!bash_grep_is_symbol_shaped(r#"grep -rn "TODO:" ."#));
        assert!(!bash_grep_is_symbol_shaped("ls -la && cargo build"));
        assert!(!bash_grep_is_symbol_shaped("cat foo.rs"));
    }

    /// #2022 witness: the exact observed pathological shape
    /// (`sleep 300; echo done`, `sleep 120` alone) is caught and its
    /// accumulated seconds are named, so the advisory can fire on it.
    #[test]
    fn bare_sleep_is_detected_and_summed() {
        assert_eq!(bare_sleep_seconds("sleep 300; echo done"), Some(300));
        assert_eq!(bare_sleep_seconds("sleep 120"), Some(120));
        assert_eq!(bare_sleep_seconds("sleep 60"), Some(60));
        // Multiple bare sleeps in one call accumulate.
        assert_eq!(
            bare_sleep_seconds("sleep 30 && sleep 30"),
            Some(60),
            "accumulated sleep across the whole call, not just the last segment"
        );
        assert_eq!(
            bare_sleep_seconds("sleep 2.5"),
            Some(3),
            "rounds to the nearest second"
        );
    }

    /// The legitimate case must stay unflagged: a sleep inside a retry
    /// backoff, or any command sharing the line with real work, is not the
    /// pathological shape — only a command whose ENTIRE body is sleep-plus-
    /// no-op is.
    #[test]
    fn a_sleep_beside_real_work_is_not_flagged() {
        assert_eq!(
            bare_sleep_seconds("sleep 2 && curl -s http://localhost:8080"),
            None
        );
        assert_eq!(bare_sleep_seconds("sleep 5; tail -f build.log"), None);
        assert_eq!(bare_sleep_seconds("read_output --wait 2"), None);
        assert_eq!(bare_sleep_seconds("echo waiting; sleep 5; ls"), None);
    }

    #[test]
    fn the_sleep_advisory_only_fires_past_the_threshold() {
        assert!(
            sleep_advisory("sleep 5").is_none(),
            "under threshold, no nudge"
        );
        let note = sleep_advisory("sleep 300; echo done").expect("over threshold");
        assert!(note.contains("300s"));
        assert!(note.contains("read_output"));
    }

    #[tokio::test]
    async fn a_cross_root_cd_warns_when_indexed() {
        let dir = indexed_tempdir();
        let out = Bash::new(None)
            .execute(&serde_json::json!({"command": "cd / && pwd"}), dir.path())
            .await;
        let text = text_of(out);
        assert!(
            text.contains("outside the session root"),
            "drift warned: {text}"
        );
    }

    /// The motivating shape from the telemetry: `cd` to a sibling checkout AND
    /// a symbol-shaped grep in one command. Drift takes precedence — under a
    /// tree the graph doesn't index, the grep tip would be misleading, so only
    /// the drift warning fires.
    #[tokio::test]
    async fn drift_wins_over_the_grep_tip_when_both_fire() {
        let dir = indexed_tempdir();
        // Grep /dev/null (instant, hermetic) — the advisory keys off the
        // command string's `cd` + grep pattern, not what grep actually reads,
        // so this exercises the precedence without walking `/`.
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "cd / && grep -rn \"struct Greeter\" /dev/null"}),
                dir.path(),
            )
            .await;
        let text = text_of(out);
        assert!(
            text.contains("outside the session root"),
            "drift warned: {text}"
        );
        // The drift note names graph_query (to explain coverage); the *tip* is
        // the thing suppressed. Its distinctive phrase must be absent.
        assert!(
            !text.contains("symbol/dependency lookup"),
            "the grep tip is suppressed under a drifted tree: {text}"
        );
    }

    #[tokio::test]
    async fn a_symbol_shaped_bash_grep_gets_the_graph_tip_when_indexed() {
        let dir = indexed_tempdir();
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "grep -rn \"struct Greeter\" ."}),
                dir.path(),
            )
            .await;
        let text = text_of(out);
        assert!(text.contains("graph_query"), "grep nudged: {text}");
    }

    #[tokio::test]
    async fn a_plain_command_gets_no_advisory() {
        let dir = indexed_tempdir();
        let text = text_of(
            Bash::new(None)
                .execute(&serde_json::json!({"command": "echo hi"}), dir.path())
                .await,
        );
        assert!(!text.contains("graph_query"), "{text}");
        assert!(!text.contains("outside the session root"), "{text}");
    }

    /// An un-indexed tree still warns on drift — that is the case the warning
    /// exists for. A best-of-N candidate's snapshot has no index, so gating the
    /// warning on one let a candidate `cd` out of its own workspace in silence.
    #[tokio::test]
    async fn no_index_still_warns_on_a_cross_root_cd() {
        let dir = tempfile::tempdir().unwrap();
        let text = text_of(
            Bash::new(None)
                .execute(
                    &serde_json::json!({"command": "grep -rn \"struct Foo\" . ; cd /"}),
                    dir.path(),
                )
                .await,
        );
        assert!(
            text.contains("outside the session root"),
            "drift warns without an index: {text}"
        );
        // The grep tip stays index-gated: it points at a tool this tree hasn't
        // got. The drift note must not smuggle it in by naming the graph.
        assert!(
            !text.contains("graph_query"),
            "no index, so nothing may cite the graph: {text}"
        );
    }

    /// The warning's load-bearing claim, on the tree with no index: work under
    /// the drifted target is not collected. A candidate that read only "the
    /// graph doesn't cover it" would reasonably `cd` anyway.
    #[tokio::test]
    async fn the_drift_warning_says_the_work_is_not_collected() {
        let dir = tempfile::tempdir().unwrap();
        let text = text_of(
            Bash::new(None)
                .execute(&serde_json::json!({"command": "cd / && pwd"}), dir.path())
                .await,
        );
        assert!(
            text.contains("is collected when the turn finishes"),
            "{text}"
        );
        assert!(text.contains("verification"), "{text}");
    }

    #[tokio::test]
    async fn a_plain_command_gets_no_advisory_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let text = text_of(
            Bash::new(None)
                .execute(&serde_json::json!({"command": "echo hi"}), dir.path())
                .await,
        );
        assert!(!text.contains("graph_query"), "{text}");
        assert!(!text.contains("outside the session root"), "{text}");
    }

    /// The `log-summary-date-ranges` loss, reduced: a script that hardcodes a
    /// path in the graded tree must not reach the disk. Before confinement the
    /// write succeeded, the run was aborted before adoption, and the grader
    /// read the leaked wrong intermediate — a solved task scoring zero.
    #[tokio::test]
    async fn a_write_into_the_graded_tree_never_reaches_the_disk() {
        let graded = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let leaked = graded.path().join("summary.csv");
        let tool =
            Bash::new(None).confined_to(ShellConfinement::graded_tree(graded.path().to_path_buf()));

        let text = text_of(
            tool.execute(
                &serde_json::json!({
                    "command": format!("echo today,ERROR,414 > {}", leaked.display())
                }),
                workspace.path(),
            )
            .await,
        );

        assert!(!leaked.exists(), "the graded tree was written: {text}");
        assert!(text.contains("refused before running"), "{text}");
        // The remedy names the file's address in this shell's own workspace.
        assert!(
            text.contains(&workspace.path().join("summary.csv").display().to_string()),
            "{text}"
        );
    }

    /// The same escape re-issued as the heredoc the model actually reaches for
    /// once `write_file` refuses it: the path is not a shell word at all, so a
    /// word-level check alone would let it straight through.
    #[tokio::test]
    async fn a_graded_path_buried_in_a_script_body_is_still_refused() {
        let graded = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let leaked = graded.path().join("summary.csv");
        let command = format!(
            "cat <<'EOF' > /dev/null\nignored\nEOF\nprintf 'x' > '{}'",
            leaked.display()
        );
        let tool =
            Bash::new(None).confined_to(ShellConfinement::graded_tree(graded.path().to_path_buf()));

        let text = text_of(
            tool.execute(&serde_json::json!({ "command": command }), workspace.path())
                .await,
        );

        assert!(!leaked.exists(), "the graded tree was written: {text}");
        assert!(text.contains("refused before running"), "{text}");
    }

    /// `f89p3/code-from-image` steps 83–86: the worker moved the very ref
    /// `verify_done` pins its baseline to. Refused in every session, confined
    /// or not — no task's work lives in Stella's ref namespace.
    #[tokio::test]
    async fn rewriting_stellas_own_ref_is_refused_and_leaves_the_ref_alone() {
        let repo = tempfile::tempdir().unwrap();
        let sentinel = repo.path().join("moved");
        let text = text_of(
            Bash::new(None)
                .execute(
                    &serde_json::json!({
                        "command": format!(
                            "touch {} && git update-ref {} HEAD",
                            sentinel.display(),
                            crate::verify::WITNESS_BASELINE_WORKTREE_REF
                        )
                    }),
                    repo.path(),
                )
                .await,
        );
        assert!(
            !sentinel.exists(),
            "the command ran despite naming Stella's ref namespace: {text}"
        );
        assert!(text.contains(confine::STELLA_REF_NAMESPACE), "{text}");
    }

    /// The legitimate case a blanket refusal would break: `configure-git-webserver`
    /// installs and configures system software, and that is the task, not an
    /// escape. Confinement must leave every path outside the graded tree alone.
    #[tokio::test]
    async fn a_confined_shell_still_writes_system_paths_outside_the_graded_tree() {
        let graded = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let conf = system.path().join("nginx.conf");
        let tool =
            Bash::new(None).confined_to(ShellConfinement::graded_tree(graded.path().to_path_buf()));

        let text = text_of(
            tool.execute(
                &serde_json::json!({
                    "command": format!("echo 'server {{}}' > {}", conf.display())
                }),
                workspace.path(),
            )
            .await,
        );

        assert!(
            conf.exists(),
            "a system path outside the graded tree: {text}"
        );
    }

    /// The advisory that sent `build-cython-ext` into `rm -rf /app/pyknotid`:
    /// it said work outside the root is not collected (true of the diff) and
    /// offered a remedy the agent cannot perform, while omitting that work in
    /// the candidate IS delivered to the graded tree by adoption.
    #[tokio::test]
    async fn the_drift_note_states_adoption_and_offers_a_remedy_the_agent_can_perform() {
        let graded = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let tool =
            Bash::new(None).confined_to(ShellConfinement::graded_tree(graded.path().to_path_buf()));

        let text = text_of(
            tool.execute(
                &serde_json::json!({ "command": format!("cd {} && pwd", elsewhere.path().display()) }),
                workspace.path(),
            )
            .await,
        );

        assert!(text.contains("outside the session root"), "{text}");
        assert!(
            text.contains("is delivered to"),
            "the note must state that work here reaches the graded tree: {text}"
        );
        assert!(
            !text.contains("re-root the session on it"),
            "the remedy must be one the agent can perform: {text}"
        );
    }

    /// Witness test for #2696: verify STELLA_SCRATCH is injected via constructor
    /// in a multi-threaded tokio runtime, proving the thread-local storage seam
    /// was fixed and the pure function signature approach works correctly.
    #[tokio::test(flavor = "multi_thread")]
    async fn constructor_injected_scratch_path_is_exported_to_bash() {
        let scratch_dir = tempfile::tempdir().expect("tempdir creation");
        let scratch_path = scratch_dir.path().to_path_buf();
        let bash_tool = Bash::new(Some(scratch_path.clone()));
        let dir = std::env::temp_dir();
        let result = bash_tool
            .execute(
                &serde_json::json!({"command": "echo $STELLA_SCRATCH"}),
                &dir,
            )
            .await;
        let exported = match result {
            ToolOutput::Ok { content } => content.lines().next().unwrap_or_default().to_string(),
            ToolOutput::Error { message, .. } => panic!("bash: {message}"),
        };
        assert_eq!(exported, scratch_path.to_string_lossy().to_string());
    }
}
