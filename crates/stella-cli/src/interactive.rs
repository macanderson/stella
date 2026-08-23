//! The "is a human present?" derivation, the interactive question io the
//! approvals plane prompts through, and the skills-registry command runner
//! the Command Deck's SKILLS overlay drives.
//!
//! The dispatchable tool surface is exactly the registry's built-ins plus
//! MCP and custom script tools — the CLI layers none of its own on top. What
//! lives here instead is what every interactive plane needs:
//!
//! - [`human_can_answer`] / [`human_is_present`] — the single derivation of
//!   "is a human present to answer?", read by the #2676 approval responder
//!   and the rules-enforcement prompt. The staged pipeline's own approval
//!   capability used to read it too, before the pipeline was removed (#3865).
//! - [`AskUserIo`] / [`TtyAskUserIo`] — how an interactive question (an
//!   approval, a scope-review confirm) reaches the human. Injectable so the
//!   decision logic is testable without a TTY: the approvals plane gets the
//!   TTY io, the deck its own card-backed implementation, tests a script.
//! - [`SkillRegistry`] — the tokenized registry-CLI runner behind the deck's
//!   user-driven skill search/install/preview flows (a human at the deck, not
//!   a model tool).

/// **Whether a human is present to answer a question this run asks
/// mid-turn.** The single derivation of that fact; every consumer reads it
/// from here rather than re-deriving it.
///
/// Asking costs a human two capabilities, and both are needed: they must SEE
/// the question (it is printed to stdout) and they must be able to ANSWER it
/// (it is read from stdin). So a run has a human exactly when it renders the
/// interactive text output *and* both stdio handles are real terminals. A
/// `--output-format text` run with a piped or redirected stdin — an ordinary
/// CI and wrapper shape — has nobody: the prompt goes out and the answer
/// never comes back.
///
/// Pure over already-observed booleans so the condition is directly
/// unit-testable without faking a terminal; [`human_is_present`] is the half
/// that reads this process's handles. The consumer today is
/// [`crate::approval::attach_interactive_approvals`] (the #2676 approval
/// responder) — the staged pipeline's own approval port was a second
/// consumer of this same fact until it was removed (#3865). Deriving it
/// separately is what once let two consumers disagree about whether anyone
/// was listening.
///
/// A thin wrapper over [`stella_tty::human_can_answer`] — every consumer in
/// this crate prints its prompt on stdout, so `stdout_is_terminal` is the
/// right name for the third input here, but the underlying predicate is
/// shared with `stella-model`'s credential prompt and the daemon console's
/// stderr-rendered scope review too (#3036), neither of which could depend on
/// this crate to read it.
pub fn human_can_answer(
    interactive_output: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> bool {
    stella_tty::human_can_answer(interactive_output, stdin_is_terminal, stdout_is_terminal)
}

/// [`human_can_answer`] against this process's real stdio handles.
pub fn human_is_present(interactive_output: bool) -> bool {
    use std::io::IsTerminal;
    human_can_answer(
        interactive_output,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
}

/// The label the runtime appends as the always-present free-text option on an
/// interactive question card: whatever the structured options are, the human
/// can always answer in their own words, and the affordance is appended by
/// the runtime, never listed by the asker.
///
/// Re-exported rather than defined here: the Command Deck's question overlay
/// ([`stella_tui::views::question`]) draws the same row, and `stella-tui`
/// cannot depend on this crate — so the one copy lives in `stella-protocol`
/// beside the question types both surfaces render.
pub use stella_protocol::FREE_TEXT_LABEL;

/// How an interactive question — an approval (#2676), a scope-review confirm
/// — reaches the human. Injectable so the decision logic is unit-testable
/// without a TTY: the approvals plane hands out [`TtyAskUserIo`], the deck
/// its own card-backed implementation, and tests a script.
#[async_trait::async_trait]
pub trait AskUserIo: Send + Sync {
    /// Present `question` + `options` (already including any free-text
    /// affordance as the final entry) and return the user's raw line.
    async fn prompt(&self, question: &str, options: &[String]) -> Result<String, String>;
}

/// Production io: prints the card to stdout and reads one line from stdin.
/// Safe to use while a turn is in flight — the REPL's own read loop is
/// suspended awaiting the turn, so stdin has exactly one reader.
pub struct TtyAskUserIo;

#[async_trait::async_trait]
impl AskUserIo for TtyAskUserIo {
    async fn prompt(&self, question: &str, options: &[String]) -> Result<String, String> {
        use std::io::Write as _;

        use colored::Colorize as _;
        println!("\n  {} {}", "?".bright_cyan().bold(), question.bold());
        for (i, option) in options.iter().enumerate() {
            println!("    {} {}", format!("{})", i + 1).bright_cyan(), option);
        }
        print!("  {} ", "answer (number or text):".dimmed());
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        // Blocking stdin read off the async runtime's worker threads.
        tokio::task::spawn_blocking(|| {
            use std::io::BufRead as _;
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map(|_| line)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// Registry commands for the skills ecosystem, driven by the Command Deck's
/// SKILLS overlay (search, install, and the ctrl+o preview) — a human-paced
/// UI flow, never a model tool. Tokenized argv templates —
/// `{query}`/`{id}` substitute as a SINGLE argv token, spawned without a
/// shell, so external text can never inject. Defaults target the
/// `npx skills` registry CLI; overridable via STELLA_SKILLS_SEARCH_CMD /
/// STELLA_SKILLS_INSTALL_CMD since registry CLIs vary.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    pub search_cmd: Vec<String>,
    pub install_cmd: Vec<String>,
    /// `npx skills use <id>` — prints a not-yet-installed skill's `SKILL.md`
    /// (wrapped in `<SKILL.md>…</SKILL.md>`); used for the ctrl+o preview.
    pub use_cmd: Vec<String>,
    pub workspace_root: std::path::PathBuf,
}

impl SkillRegistry {
    pub fn from_env(workspace_root: std::path::PathBuf) -> Self {
        let parse = |var: &str, default: &[&str]| -> Vec<String> {
            std::env::var(var)
                .ok()
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
        };
        Self {
            search_cmd: parse(
                "STELLA_SKILLS_SEARCH_CMD",
                &["npx", "skills", "find", "{query}"],
            ),
            install_cmd: parse(
                "STELLA_SKILLS_INSTALL_CMD",
                // `--yes` skips the interactive "which agents do you want to install
                // to?" multi-select that the registry CLI otherwise shows — it would
                // hang forever when stella runs it with stdin null. `-y` is the short
                // form the CLI prints in its own hint ("use --yes (-y) … to install
                // without prompts").
                &["npx", "skills", "add", "--yes", "{id}"],
            ),
            use_cmd: parse("STELLA_SKILLS_USE_CMD", &["npx", "skills", "use", "{id}"]),
            workspace_root,
        }
    }

    /// Substitute `placeholder` with `value` across the template's tokens.
    pub fn render(template: &[String], placeholder: &str, value: &str) -> Vec<String> {
        template
            .iter()
            .map(|t| t.replace(placeholder, value))
            .collect()
    }

    pub(crate) async fn run(&self, argv: Vec<String>, timeout_secs: u64) -> Result<String, String> {
        // Search/install output is a terse list — 6 KB is ample and guards the
        // debug log / UI from a runaway subprocess.
        self.run_capped(argv, timeout_secs, 6000).await
    }

    /// Like [`Self::run`] but with an explicit output cap. The ctrl+o preview
    /// passes a larger cap so a full `SKILL.md` body is not truncated.
    pub(crate) async fn run_capped(
        &self,
        argv: Vec<String>,
        timeout_secs: u64,
        max_bytes: usize,
    ) -> Result<String, String> {
        let Some((program, args)) = argv.split_first() else {
            return Err("empty registry command".into());
        };
        // kill_on_drop: when the timeout below fires, wait_with_output is
        // dropped and the child must die with it — otherwise a wedged npx
        // install keeps running (and downloading) long after the tool
        // reported failure.
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(&self.workspace_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // The executable/template is environment configurable. It gets
        // ordinary PATH/build configuration, never provider, repository,
        // cloud, or tracker credentials.
        scrub_skill_registry_command(&mut command);
        let child = command
            .spawn()
            .map_err(|e| format!("failed to run `{program}`: {e} (is Node/npx installed?)"))?;
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| format!("`{program}` timed out after {timeout_secs}s"))?
        .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut text = format!("{}\n{}", stdout.trim(), stderr.trim());
        if text.len() > max_bytes {
            // Truncate on a char boundary — `String::truncate` panics if the
            // cap lands inside a multi-byte character (subprocess output can
            // contain accents/emoji/box-drawing), which would unwind the whole
            // CLI session.
            let end = (0..=max_bytes)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(0);
            text.truncate(end);
        }
        if output.status.success() {
            Ok(text.trim().to_string())
        } else {
            Err(format!("exit {}: {}", output.status, text.trim()))
        }
    }
}

fn scrub_skill_registry_command(command: &mut tokio::process::Command) {
    stella_tools::subprocess_env::scrub_sensitive_env(command);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skill_registry_child_cannot_receive_explicit_credentials() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s|%s|%s|%s' \"${OPENROUTER_API_KEY-unset}\" \"${GITHUB_TOKEN-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"${STELLA_TEST_BENIGN-unset}\"",
            ])
            .env("OPENROUTER_API_KEY", "provider-secret")
            .env("GITHUB_TOKEN", "repository-secret")
            .env("AWS_SECRET_ACCESS_KEY", "cloud-secret")
            .env("STELLA_TEST_BENIGN", "visible");
        scrub_skill_registry_command(&mut command);

        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "unset|unset|unset|visible"
        );
    }

    /// The default install command must carry `--yes`: `npx skills add` is
    /// otherwise an interactive multi-select that hangs forever when stella
    /// runs it with stdin null. Locking the default here guards against a
    /// regression that would silently re-break TUI installs.
    #[test]
    fn default_install_cmd_is_non_interactive() {
        // Clear any override so the default path is exercised.
        let _lock = crate::test_env::lock();
        unsafe {
            std::env::remove_var("STELLA_SKILLS_INSTALL_CMD");
        }
        let reg = SkillRegistry::from_env(std::path::PathBuf::from("/ws"));
        assert!(
            reg.install_cmd.iter().any(|t| t == "--yes" || t == "-y"),
            "default install cmd must be non-interactive (have --yes/-y): {:?}",
            reg.install_cmd
        );
        // The {id} placeholder is still present for substitution.
        assert!(reg.install_cmd.iter().any(|t| t.contains("{id}")));
    }

    /// **The witness for the one-fact fix.** `--output-format text` with a
    /// stdin that is not a terminal — an ordinary CI/wrapper invocation — is
    /// the configuration where two independent derivations used to disagree.
    /// Every remaining consumer reads [`human_can_answer`], so all say no.
    #[tokio::test]
    async fn a_piped_stdin_text_run_denies_every_consumer_of_the_human_present_fact() {
        // Interactive text output, stdin redirected, stdout still a terminal.
        let present = human_can_answer(true, false, true);
        assert!(!present, "a piped stdin cannot answer a mid-turn question");

        // Consumer 1 — the #2676 approval responder is not attached, so the
        // broker refuses with the grant-path message instead of parking on a
        // stdin nobody is holding.
        //
        // Asked through `MidTurnAsk::tty_when`, which is where the fact is
        // turned into a posture since #4220 replaced the boolean parameter.
        // That is the seam the production callers use, so this is the same
        // question they ask rather than a re-derivation beside it.
        assert!(
            matches!(
                crate::rules::MidTurnAsk::tty_when(present),
                crate::rules::MidTurnAsk::Headless
            ),
            "no human means no surface to attach"
        );
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let registry = stella_tools::ToolRegistry::new(workspace.path().to_path_buf());
        let outcome = registry
            .approval_broker()
            .resolve(None, &approval_request())
            .await;
        match outcome {
            stella_tools::registry::approval::ApprovalOutcome::Denied { reason } => assert!(
                reason.contains("no interactive surface"),
                "the broker must stay headless: {reason}"
            ),
            other => panic!("no human, so nothing may approve: {other:?}"),
        }
    }

    fn approval_request() -> stella_tools::registry::approval::ApprovalRequest {
        stella_tools::registry::approval::ApprovalRequest {
            parked: stella_tools::registry::approval::ApprovalSubject::Tool {
                name: "delegate".into(),
                read_only: false,
            },
            reason: "spawns need a human".into(),
            gate: "tool.call.requested".into(),
            subject: Some("delegate: refactor the parser".into()),
        }
    }
}
