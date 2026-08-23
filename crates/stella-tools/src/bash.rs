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
//! **The per-command boundary is that audit; there is no OS sandbox here.**
//! `STELLA_BASH_SANDBOX`, the opt-in Seatbelt/`bwrap` wrapper, was removed in
//! #1300 for claiming a session-wide bound it never had — it wrapped this one
//! tool while every other spawn path ran around it. The `confine`/`contain`
//! pair that replaced it put a kernel-level write ban on a graded tree, and it
//! is not coming back: it was armed by the candidate-workspace registry alone,
//! which this crate no longer builds. That costs a real guarantee — a computed
//! path (`chr(47)`) walks past any audit of the command text, and the kernel
//! write ban is what used to refuse it anyway. The decision and its price are
//! in `doc:remote-sandboxes` §2.5. Session isolation belongs to the container
//! the whole Stella process runs in (`doc:remote-sandboxes` §2).

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use crate::registry::Tool;

mod words;

use stella_core::shell_text::bare_sleep_seconds;
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
    //
    // Which is why this half — and only this half — scans the command with its
    // non-executed regions removed (#3618). A heredoc body or a `#` comment
    // naming a hidden path reads nothing, and refusing for it hands the agent
    // a scope error about a path it never asked for. Every way
    // `strip_data_regions` degrades loses text, so the worst it can do here is
    // miss a refusal this half was never able to guarantee anyway. The write
    // loop below keeps the raw words, where losing text is the unsafe
    // direction.
    for word in &shell_words(&words::strip_data_regions(command)) {
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
/// The system temp directory is deliberately **not** here either, and this
/// time not because it is refused: it is a genuine scope root now
/// ([`crate::temp_roots`]), so `ctx.refuse_write` allows it on the same
/// evidence as the workspace, and listing it as an exemption would be a second
/// spelling of one boundary — the shape that lets `write_file` and `bash`
/// disagree about the same path.
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

/// The advisory footer for a bash result: a cross-root `cd` warning, and
/// nothing else.
///
/// **A bash result carries no tool-preference advice, ever.** There used to be
/// a second note here, appended whenever a grep pattern looked symbol-shaped,
/// pointing the model at [`crate::search`]. It was measured on a 20-task
/// Terminal-Bench panel and it was pure loss: it fired on **44 of 415 tool
/// results across 10 of 20 tasks** and produced **zero** `search` calls. It
/// fired on hardware probes (`cat /proc/cpuinfo | grep -i vmx`), on package
/// listings (`dpkg -L qemu-system-x86 | grep bin`), on a grep of a C header —
/// none of them symbol hunts. The predicate keyed on a bare identifier after a
/// grep word, and in a benchmark container (`/app`, not a repository) there is
/// nothing to search in the first place.
///
/// The lesson generalizes past that one predicate, which is why the machinery
/// is deleted rather than narrowed: advice injected into tool *output* is
/// re-sent as input on every later turn of the trial, so a wrong nudge is a
/// context tax that compounds, and the model has no way to tell engine prose
/// from the command's own bytes. Tool preference belongs in the tool schema
/// and the system prompt, which the model reads once. Do not reintroduce a
/// nudge here in any form.
///
/// The `cd` warning stays, and is a different thing: it reports a *fact about
/// this command's effect* — work outside the session root is not collected —
/// rather than an opinion about which tool should have been called.
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
    None
}

/// The per-call half of #2022: a bare `sleep` long enough to be worth naming
/// in the result the model reads.
///
/// Deliberately the cheap rung, and deliberately low — 30s catches the shape
/// on the very call that made it, before any accumulation. What it cannot see
/// is the *turn*: loop detection reads interleaved calls as progress and the
/// budget guard is spend-based, so idling costs $0. That blind spot is the
/// engine's to close, over the seconds a whole turn has asked for
/// (`stella_core`'s stall rung, `driver::loop_escalation`), and this advisory
/// is not it.
///
/// Honest visibility, not a refusal, on either rung: a static text-shape check
/// on the command ([`bare_sleep_seconds`]), never a measured elapsed time, so
/// it stays deterministic for the loop detector (never embed a timing here;
/// see `stella-tool-timings-must-not-ride-tooloutput`).
const SLEEP_ADVISORY_THRESHOLD_SECS: u64 = 30;

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
        // Advertised here, and only when the plane actually initialized,
        // because the confinement error is otherwise the *first* place a model
        // learns this directory exists — it has to fail a write to find out.
        // Measured on a Terminal-Bench trial: the agent needed a C file it
        // could compile, was refused at `/tmp`, and fell back to writing the
        // shim into the graded workspace — exactly the litter the confinement
        // exists to prevent, and exactly what this sentence prevents instead.
        //
        // The variable name is named, never the path: the path carries a
        // per-session random suffix, and a system prompt or schema that
        // embedded it would differ byte-for-byte between sessions and cost a
        // cold prompt-cache write every time (invariant 7).
        let scratch = if self.scratch.is_some() {
            // Phrased as a usable command fragment, not as a fact to look up.
            // Measured: told only that a scratch directory existed and that
            // `get_environment` reports its path, an agent spent a turn
            // calling `get_environment` before retrying. `$STELLA_SCRATCH` is
            // already in the command's environment; saying so removes the
            // lookup entirely.
            " Redirect working files that are NOT deliverables — a captured log, a \
             compiled shim, a scratch script — to $STELLA_SCRATCH, which is already \
             exported into your shell: `apt-get update > $STELLA_SCRATCH/apt.log` \
             needs no lookup. It is writable, nothing there lands in this turn's diff, \
             and it is deleted when the session ends. (get_environment reports its \
             absolute path if a file tool needs one.)"
        } else {
            ""
        };
        ToolSchema {
            name: "bash".into(),
            description: format!(
                "Run a shell command in the workspace root. Returns stdout+stderr with a \
                timeout backstop. You can READ anything on this machine — system headers, the \
                toolchain, a dependency's source. You can only CHANGE things inside this \
                session's directories (get_environment reports the workspace root), so a \
                command that creates, edits, deletes or moves a file elsewhere is refused \
                before it runs. Prefer write_file/edit_file/delete_file over shell equivalents \
                for files in the workspace: their changes are what this turn's diff and \
                verification are computed from.{scratch}"
            ),
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
        crate::exec::detach_into_own_process_group(&mut cmd);

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

    /// A bash result never carries tool-preference advice — for any command.
    ///
    /// The deleted nudge fired on 44 of 415 tool results across a 20-task
    /// Terminal-Bench panel and produced zero `search` calls, including on
    /// every command below that is not a symbol hunt at all. The first four
    /// are the shapes the old predicate deliberately matched; the rest are
    /// shapes it matched by accident, taken verbatim from the trials it
    /// misfired on. Both groups must now be silent.
    #[test]
    fn a_bash_result_never_carries_tool_preference_advice() {
        let root = std::path::Path::new("/app");
        for command in [
            // what the old predicate meant to catch
            r#"grep -rn "struct DeckProviderResolver" stella-tools/"#,
            "grep -n ReadOnlyTools src/",
            r#"rg -e "pub fn resolve" ."#,
            r#"grep -rn "pub mod ports\|pub use ports" src/"#,
            // what it actually caught, from the measured misfires
            "ls /dev/kvm 2>&1; cat /proc/cpuinfo | grep -i vmx | head -1; uname -a",
            "dpkg -L qemu-system-x86 2>&1 | grep bin",
            r#"ls /usr/bin | grep -iE "^gcc|^cc$|^clang""#,
            "cat /usr/include/x86_64-linux-gnu/asm/unistd_64.h | grep -i epoll",
            // ordinary text scans
            r#"grep -rn "unwrap()" src/"#,
            "cat foo.rs",
        ] {
            assert_eq!(
                drift_advisory(command, root),
                None,
                "advice was appended for `{command}`"
            );
        }
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

    /// The scratch directory is advertised where the model looks, not only in
    /// the error it gets for guessing wrong.
    ///
    /// Before this, the sole mention of the path was the confinement refusal,
    /// so an agent needing a compilable working file learned of it by failing
    /// and then wrote into the graded workspace anyway. The variable name is
    /// asserted rather than the path: embedding a per-session random path in a
    /// schema costs a cold prompt-cache write every session (invariant 7).
    #[test]
    fn bash_advertises_the_scratch_directory_only_when_it_exists() {
        let with = Bash::new(Some(std::path::PathBuf::from("/tmp/stella-scratch-x")));
        let described = with.schema().description;
        assert!(
            described.contains("$STELLA_SCRATCH"),
            "bash must name the scratch variable: {described}"
        );
        assert!(
            !described.contains("stella-scratch-x"),
            "the per-session path must not reach the schema: {described}"
        );

        // No plane, no promise: never advertise a capability that is absent.
        let without = Bash::new(None);
        assert!(!without.schema().description.contains("$STELLA_SCRATCH"));
    }

    /// End-to-end counterpart to the unit witness above: a real `bash` call
    /// whose command is the most symbol-shaped grep there is comes back with
    /// the command's own bytes and nothing appended.
    ///
    /// This assertion is deliberately the inverse of the one it replaces. That
    /// test asserted the nudge fired even on an unindexed tree, and it was
    /// right about the mechanism and wrong about the value: measured over a
    /// 20-task panel the nudge converted 44 firings into zero `search` calls
    /// while taxing the context of every later turn in those trials.
    #[tokio::test]
    async fn a_symbol_shaped_bash_grep_comes_back_unannotated() {
        let dir = tempfile::tempdir().unwrap();
        let out = Bash::new(None)
            .execute(
                &serde_json::json!({"command": "grep -rn \"struct Greeter\" ."}),
                &cx(dir.path()),
            )
            .await;
        let text = text_of(out);
        assert!(
            !text.contains("`search` answers those directly"),
            "a bash result must carry no tool-preference advice: {text}"
        );
        assert!(
            !text.contains("note: that pattern"),
            "a bash result must carry no tool-preference advice: {text}"
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
