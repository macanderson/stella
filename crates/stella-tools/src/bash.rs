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
//! # Read the machine; change only what this session owns
//!
//! The policy is [`stella_core::workspace_scope`]'s and the reasoning is
//! there. In short: reads reach the whole filesystem, because an agent fixing
//! a build needs system headers, the toolchain and a dependency's source, and
//! a read cannot damage the user's tree. Writes are confined to the session's
//! workspace plus operator-granted directories. The one thing hidden from
//! *both* is the origin project's tree — a worktree session cannot see the
//! project it was cut from, and no session can see its own worktrees, because
//! a parallel checkout answers questions about the wrong copy of the file
//! being edited.
//!
//! `shell_write_audit` (crate-private) applies that to the command text
//! **before the spawn**, and its own header is the honest account of what a
//! text audit can and cannot do. The short version: it is a fence against the
//! mistake that actually happens (a copied absolute path, an `rm` aimed at the
//! wrong tree, a redirect into a sibling checkout), it is **not** a sandbox,
//! and it is biased hard toward permitting — a false refusal breaks a build
//! for a reason the agent cannot diagnose, which costs more than the rare
//! escape it would have caught. Confinement is *enforced* in the file tools,
//! which resolve a path and hold a descriptor.
//!
//! **There is still no session-wide OS sandbox here.** `STELLA_BASH_SANDBOX`,
//! the opt-in Seatbelt/`bwrap` wrapper, was removed in #1300 for claiming a
//! session-wide bound it never had — it wrapped this one tool while every
//! other spawn path ran around it. The `confine`/`contain` pair that replaced
//! it put a kernel-level write ban on a graded tree, but was armed only by the
//! candidate-workspace registry this crate no longer builds; restoring it
//! without a caller would be unwired code, so it is tracked in #3468 rather
//! than shipped dark. Session isolation belongs to the container the whole
//! Stella process runs in (`docs/spec/remote-sandboxes.md` §2).

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use crate::registry::Tool;

mod words;

use words::{cd_escape_target, is_operator_word, shell_words};

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
/// sit far apart — while the `exec` budget bounds one incremental page of a
/// long-running capture. (That budget's original consumer was the managed
/// process family's `read_output`, deleted in #3244; the ratio argument is
/// about the two *shapes* of output, so it survives its example.) Aligning
/// them would either starve the shell or inflate every page read.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// grep-family commands whose first positional arg is a search pattern.
const GREP_CMDS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ripgrep", "ag"];

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
            if is_operator_word(next) {
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

/// Commands whose positional arguments name things they **change**.
///
/// Deliberately short, and deliberately only commands whose argument shape is
/// unambiguous. A longer list is not a safer one: every entry is a chance to
/// misread a flag as a path and refuse a legitimate build step, and a shell
/// audit that cries wolf gets switched off, at which point it protects
/// nothing. The commands here cover what actually destroys work in practice.
const MUTATING_COMMANDS: &[&str] = &[
    "rm", "rmdir", "touch", "mkdir", "truncate", "chmod", "chown", "shred",
];

/// Commands whose **last** positional is the thing they change, and whose
/// earlier positionals are sources they only read.
///
/// Split out from [`MUTATING_COMMANDS`] because scanning every argument of
/// these refuses the read half: `cp /usr/share/doc/example.toml ./config.toml`
/// copies *from* a system path *into* the workspace, which is exactly the
/// read the tool's own schema promises is unrestricted. Refusing it would be
/// a false positive on one of the most ordinary commands there is.
///
/// `dd` is deliberately absent from both lists: its target is `of=…` rather
/// than a positional, so neither shape describes it and pretending otherwise
/// would be a rule that reads the wrong argument. It is covered by
/// [`ASSIGNMENT_TARGET_COMMANDS`] instead.
const LAST_ARG_MUTATING_COMMANDS: &[&str] = &["mv", "cp", "install", "ln"];

/// Commands that name their write target with a `key=value` argument rather
/// than a positional — `dd of=/etc/passwd` being the one that matters.
const ASSIGNMENT_TARGET_COMMANDS: &[&str] = &["dd"];

/// What the shell audit concluded about one command.
///
/// `None` means "nothing resolvable pointed outside the scope" — which is the
/// answer for the overwhelming majority of commands, including every one whose
/// paths this scanner cannot resolve. That is the deliberate bias: see
/// [`shell_write_audit`].
///
/// # What this is, and what it is not
///
/// It is a **text audit of the command the model wrote**, performed before the
/// spawn. It catches the copied absolute path, the `rm -rf /some/other/tree`,
/// the redirect into a sibling checkout — the mistakes that actually happen.
///
/// It is **not a sandbox**, and nothing here should be described as one. A
/// shell can compute a path (`$(printf '\057etc')`), read one from a file, or
/// run a script that does either, and no amount of scanning the command text
/// survives that. The file tools are where confinement is *enforced*, because
/// they resolve a path and hold a descriptor; this is a fence that stops
/// honest mistakes and says so.
///
/// The bias is toward permitting, and that is a benchmarking decision as much
/// as a correctness one: a false refusal breaks a build for a reason the agent
/// cannot diagnose, and one of those costs more than the rare escape this
/// would have caught.
fn shell_write_audit(command: &str, ctx: &crate::ctx::ToolCtx) -> Option<String> {
    let words = shell_words(command);
    let mut index = 0usize;

    // The denied tree is refused for READING too, which is the one place this
    // audit checks a non-write. `stella_core::workspace_scope` hides the origin
    // project from a worktree session, and hides worktrees from everyone else,
    // because a parallel checkout answers questions about the wrong tree. That
    // reason applies just as much to `cat ../../src/main.rs` as it does to
    // `read_file` — so any argument naming a hidden path stops the command,
    // whatever the command is.
    //
    // Unlike the write half this cannot be complete: a shell can compute the
    // path, or `cd` and use a relative one. It closes the accidental case,
    // which is the one that actually happens.
    for word in &words {
        if word.starts_with('-') || word.contains('$') || word.contains('*') {
            continue;
        }
        if let Some(refusal) = ctx.refuse_read(word) {
            return Some(refusal);
        }
    }

    while index < words.len() {
        let word = words[index].as_str();

        // A redirect names a file the shell itself will create or truncate,
        // whatever the command around it is. `2>`, `&>` and `>|` are the same
        // operator wearing a file descriptor or a clobber flag, so the leading
        // digits and the trailing `|` come off before the target is read —
        // without that, `make 2>/etc/x` reads as an ordinary word and is never
        // checked at all.
        if let Some(target) = redirect_target(word) {
            let target = if target.is_empty() {
                words.get(index + 1).map(String::as_str).unwrap_or("")
            } else {
                target
            };
            if let Some(refusal) = refuse_if_outside(target, ctx) {
                return Some(refusal);
            }
        }

        // Every positional is a target.
        if MUTATING_COMMANDS.contains(&word)
            // `sed -i` and `tee` edit their positionals in place; for `sed`
            // the in-place flag is what separates a write from a read. Both
            // spellings, because `--in-place` is not `-i`.
            || (word == "sed"
                && segment_args(&words, index)
                    .iter()
                    .any(|w| w.starts_with("-i") || *w == "--in-place"))
            || word == "tee"
        {
            for argument in segment_args(&words, index) {
                if argument.starts_with('-') {
                    continue;
                }
                if let Some(refusal) = refuse_if_outside(argument, ctx) {
                    return Some(refusal);
                }
            }
        }

        // Only the LAST positional is a target; the rest are sources this
        // command merely reads, and reads are unrestricted.
        if LAST_ARG_MUTATING_COMMANDS.contains(&word)
            && let Some(target) = segment_args(&words, index)
                .into_iter()
                .rfind(|argument| !argument.starts_with('-'))
            && let Some(refusal) = refuse_if_outside(target, ctx)
        {
            return Some(refusal);
        }

        // The target rides an assignment rather than a positional (`dd of=…`).
        if ASSIGNMENT_TARGET_COMMANDS.contains(&word) {
            for argument in segment_args(&words, index) {
                if let Some(target) = argument.strip_prefix("of=")
                    && let Some(refusal) = refuse_if_outside(target, ctx)
                {
                    return Some(refusal);
                }
            }
        }

        index += 1;
    }
    None
}

/// The file a redirect writes to, if `word` is one.
///
/// Handles the spellings a naive `strip_prefix('>')` misses, each of which is
/// an ordinary thing to write and would otherwise sail past the audit
/// unchecked: a file-descriptor prefix (`2>`, `1>>`), the `&>` both-streams
/// form, and the `>|` clobber override. `>&` is deliberately NOT a redirect to
/// a file — `2>&1` duplicates a descriptor and names no path.
fn redirect_target(word: &str) -> Option<&str> {
    let after_fd = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let after_fd = after_fd.strip_prefix('&').unwrap_or(after_fd);
    let rest = after_fd
        .strip_prefix(">>")
        .or_else(|| after_fd.strip_prefix('>'))?;
    // `2>&1` and `>&2` duplicate a descriptor; there is no file here.
    if rest.starts_with('&') {
        return None;
    }
    Some(rest.strip_prefix('|').unwrap_or(rest))
}

/// This command's own arguments: everything up to the next operator.
///
/// A pipeline or `&&` ends a command's reach — without that bound,
/// `rm x && cd /tmp` would read `/tmp` as something `rm` was about to delete.
fn segment_args(words: &[String], command_index: usize) -> Vec<&str> {
    words[command_index + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|word| !is_operator_word(word))
        .collect()
}

/// Redirect targets that **discard or re-emit** output rather than writing a
/// file, and are therefore never the loss this boundary exists to prevent.
///
/// This is the whole exemption list, and it is deliberately four character
/// devices rather than a directory. `command -v jq >/dev/null 2>&1` is the
/// single most common idiom in shell scripting; refusing it makes the shell
/// unusable for capability probing while protecting nothing, because nothing
/// is stored. `/dev/stdout` and `/dev/stderr` write to the pipes Stella is
/// already capturing.
///
/// The system temp directory is deliberately **not** here. It was, briefly,
/// on the argument that `/tmp` is not the user's work — but the rule this
/// module enforces is that a session writes inside its own directories, and
/// `/tmp` is outside them. A session that needs scratch space has a scratch
/// directory of its own (`STELLA_SCRATCH`, granted as a scope root by
/// `ToolRegistry::write_scope`), which is where that work belongs and which
/// is cleaned up with the session. An operator who genuinely wants `/tmp`
/// writable says so with `--allow-dir /tmp`.
const ALWAYS_WRITABLE: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

/// The refusal for one argument, when it resolves to something outside the
/// session's writable directories.
///
/// Anything the scanner cannot resolve to a concrete path — a variable, a
/// glob, a home-relative `~`, an option-looking token — is **skipped**, not
/// guessed at. A wrong refusal is worse than a missed one here (see
/// [`shell_write_audit`]).
fn refuse_if_outside(argument: &str, ctx: &crate::ctx::ToolCtx) -> Option<String> {
    if argument.is_empty()
        || argument.starts_with('$')
        || argument.starts_with('~')
        || argument.starts_with('-')
        || argument.contains('*')
        || argument.contains('?')
        || argument.contains('$')
    {
        return None;
    }
    if ALWAYS_WRITABLE.contains(&argument) {
        return None;
    }
    ctx.refuse_write(argument)
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
/// loop detection sees other calls between the sleeps, so the
/// interleaved-repeat rung reads the window as progressing, and the budget
/// guard is spend-based, so idling costs $0. (The shape #2022 observed
/// interleaved `read_output` polls; that tool is gone, but any interleaved
/// call produces the same blind spot, so the reasoning is unchanged.) #2022's first step is honest
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
        if is_operator_word(w) {
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

/// A footer naming a bare `sleep` that crossed the advisory threshold, and a
/// remedy the agent can actually perform.
///
/// **The remedy has to name a tool that exists.** This advisory shipped for a
/// while pointing at `read_output`/`wait_for` — the managed-process family,
/// which #3244 deleted and the tool restore did not bring back. Every long
/// bare `sleep` therefore handed the model a directive with no tool behind it,
/// which is worse than the silence it replaced: an instruction that cannot be
/// followed teaches the model to discount the next one too. The text was
/// restored verbatim along with the rest of `bash`, and the stale half was
/// only caught by an issue sweep afterwards.
///
/// So the wording now stays inside what the surface offers: a short poll in a
/// loop, which `bash` can do on its own, and which returns as soon as the
/// condition holds instead of blocking the whole interval.
fn sleep_advisory(command: &str) -> Option<String> {
    let secs = bare_sleep_seconds(command)?;
    if secs < SLEEP_ADVISORY_THRESHOLD_SECS {
        return None;
    }
    Some(format!(
        "\n\nnote: this call blocked for {secs}s inside a bare `sleep` with no other work in \
         it, and the whole interval was charged to the turn whether or not the thing you are \
         waiting for finished early. If you are waiting on something, poll for the condition \
         instead of sleeping through it — a bounded retry loop that checks and exits as soon \
         as the check passes (for example `for i in $(seq 30); do <check> && break; sleep 1; \
         done`) costs a fraction of a blind wait."
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
            description: "Run a shell command in the workspace root. Returns stdout+stderr with a \
                timeout backstop. You can READ anything on this machine — system headers, the \
                toolchain, a dependency's source. You can only CHANGE things inside this \
                session's directories (get_environment reports the workspace root), so a \
                command that creates, edits, deletes or moves a file elsewhere is refused \
                before it runs. Prefer write_file/edit_file/delete_file over shell equivalents \
                for files in the workspace: their changes are what this turn's diff and \
                verification are computed from."
                .into(),
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
        // Audited before the spawn, on the text the model wrote: a refusal
        // that arrives after the process has run is a report, not a fence.
        // Read `shell_write_audit` on why this permits far more than it
        // refuses, and on why it is not a sandbox.
        if let Some(refusal) = shell_write_audit(command, ctx) {
            return ToolOutput::error(refusal);
        }

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

    /// A bare execution context rooted at `root`: `Tool::execute` takes the
    /// context rather than the bare root path it used to (#3284).
    fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
        crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
    }

    #[tokio::test]
    async fn runs_echo_command() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "echo hello_stella"}),
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("hello_stella")),
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
                &cx(dir.path()),
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
                &cx(std::env::temp_dir()),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
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
                &cx(std::env::temp_dir()),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
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
                &cx(&dir),
            )
            .await;
        let elapsed = started.elapsed();
        match result {
            ToolOutput::Ok { content, .. } => {
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
            .execute(&serde_json::json!({"command": "echo err >&2"}), &cx(&dir))
            .await;
        match result {
            ToolOutput::Ok { content, .. } => assert!(content.contains("err")),
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
        }
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let dir = std::env::temp_dir();
        let result = Bash::new(None)
            .execute(&serde_json::json!({"command": "exit 42"}), &cx(&dir))
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
                &cx(&dir),
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
                    &cx(&root),
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
                &cx(&dir),
            )
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
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
            .execute(&serde_json::json!({ "command": command }), &cx(&dir))
            .await;
        let content = match result {
            ToolOutput::Ok { content, .. } => content,
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
            .execute(&serde_json::json!({"command": "pwd"}), &cx(&dir))
            .await;
        match result {
            ToolOutput::Ok { content, .. } => {
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

    fn text_of(out: ToolOutput) -> String {
        match out {
            ToolOutput::Ok { content, .. } => content,
            ToolOutput::Error { message, .. } => message,
        }
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
        // The remedy must be one the agent can actually perform. This
        // assertion used to require the string `read_output` — a tool #3244
        // deleted — so it kept a directive with no tool behind it green for
        // as long as it existed. Now it pins that the note names polling,
        // and that it names NO tool the catalog does not carry.
        assert!(note.contains("poll"), "{note}");
        for gone in ["read_output", "wait_for", "start_process"] {
            assert!(
                !note.contains(gone),
                "the advisory names `{gone}`, which is not on the tool surface: {note}"
            );
        }
    }

    #[tokio::test]
    async fn a_cross_root_cd_warns() {
        let dir = tempfile::tempdir().unwrap();
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "cd / && pwd"}),
                &cx(dir.path()),
            )
            .await;
        let text = text_of(out);
        assert!(
            text.contains("outside the session root"),
            "drift warned: {text}"
        );
    }

    /// The motivating shape from the telemetry: `cd` to a sibling checkout AND
    /// a symbol-shaped grep in one command. Drift takes precedence — the
    /// search tip is advice about working *here*, which is exactly what the
    /// command just stopped doing.
    #[tokio::test]
    async fn drift_wins_over_the_search_tip_when_both_fire() {
        let dir = tempfile::tempdir().unwrap();
        // Grep /dev/null (instant, hermetic) — the advisory keys off the
        // command string's `cd` + grep pattern, not what grep actually reads,
        // so this exercises the precedence without walking `/`.
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "cd / && grep -rn \"struct Greeter\" /dev/null"}),
                &cx(dir.path()),
            )
            .await;
        let text = text_of(out);
        assert!(
            text.contains("outside the session root"),
            "drift warned: {text}"
        );
        assert!(
            !text.contains("`search` answers those directly"),
            "the search tip is suppressed under a drifted tree: {text}"
        );
    }

    /// The witness for the retargeted nudge: a symbol-shaped grep is pointed
    /// at `search`, and — unlike the `graph_query` tip this replaces — it
    /// fires on a tree with **no index at all**, because `search`'s bottom
    /// rung is an index-free file scan. The old tip was index-gated for the
    /// honest reason that `graph_query` did not answer without one; that
    /// reason is gone, so the gate is too.
    #[tokio::test]
    async fn a_symbol_shaped_bash_grep_is_pointed_at_search_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "grep -rn \"struct Greeter\" ."}),
                &cx(dir.path()),
            )
            .await;
        let text = text_of(out);
        assert!(
            text.contains("`search` answers those directly"),
            "grep nudged toward search: {text}"
        );
    }

    /// The warning's load-bearing claim: work under the drifted target is not
    /// collected. An agent that read only "the graph doesn't cover it" would
    /// reasonably `cd` anyway.
    #[tokio::test]
    async fn the_drift_warning_says_the_work_is_not_collected() {
        let dir = tempfile::tempdir().unwrap();
        let text = text_of(
            Bash::new(None)
                .execute(
                    &serde_json::json!({"command": "cd / && pwd"}),
                    &cx(dir.path()),
                )
                .await,
        );
        assert!(
            text.contains("is collected when the turn finishes"),
            "{text}"
        );
        assert!(text.contains("verification"), "{text}");
    }

    #[tokio::test]
    async fn a_plain_command_gets_no_advisory() {
        let dir = tempfile::tempdir().unwrap();
        let text = text_of(
            Bash::new(None)
                .execute(&serde_json::json!({"command": "echo hi"}), &cx(dir.path()))
                .await,
        );
        assert!(!text.contains("`search` answers those directly"), "{text}");
        assert!(!text.contains("outside the session root"), "{text}");
    }
}
