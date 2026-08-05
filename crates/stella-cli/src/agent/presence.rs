//! A headless/plain session's presence in the machine-wide session registry.

use super::*;

/// A headless/plain session's presence in the machine-wide registry: the
/// deck's SESSIONS overlay finds it live and — because every execution links
/// back via [`super::begin_execution`]'s `session` — can replay it long after
/// it ended. Registration is best-effort throughout: a failed registry write
/// never disturbs the run.
pub(crate) struct SessionPresence {
    registry: stella_store::SessionRegistry,
    record: stella_store::SessionRecord,
    name: String,
    /// This session's durable record, carried so [`Self::finish`] can compact
    /// it. Cloning the handle rather than re-opening the record keeps the
    /// compaction pointed at the session that was actually bound, whatever the
    /// driver did in between.
    durability: crate::durability::SessionDurability,
}

impl SessionPresence {
    /// Announce the session (status In Progress), titled from the workspace
    /// and the prompt/goal that started it, and bind this session's durability
    /// to `tools`.
    ///
    /// The binding is done HERE, rather than left to each headless driver,
    /// because this is the moment the session acquires the identity durability
    /// is keyed on. A driver that announced without binding would run a whole
    /// session whose turns checkpoint nowhere — and the failure would be
    /// silent, because an unbound sink is indistinguishable from a session
    /// that simply never crashed.
    pub(crate) fn announce(cfg: &Config, prompt: &str, tools: &Arc<ToolRegistry>) -> Self {
        let name = cfg
            .workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| cfg.workspace_root.display().to_string());
        let registry = stella_store::SessionRegistry::open_default();
        // A supervised run continues the record its supervisor registered
        // rather than minting a second one for the same work (#1552). The
        // supervisor knew the pid and the process group; this side knows the
        // prompt and, in `finish`, the outcome — and `stella daemon list` has
        // to show one session, not two halves of one. Its `pid` is already
        // this process: the supervisor recorded the child's, deliberately.
        let mut record = crate::daemon::supervised_id()
            .and_then(|id| registry.get(&id))
            .unwrap_or_else(|| {
                stella_store::SessionRecord::new(
                    cfg.workspace_root.display().to_string(),
                    name.clone(),
                )
            });
        record.title = format!("{name}: {}", crate::command_deck::prompt_line(prompt, 48));
        record.summary = crate::command_deck::prompt_line(prompt, 240);
        let _ = registry.upsert(&record);
        // stderr, not stdout: `--output-format json` owns stdout, and a
        // durability advisory must never land inside a machine-readable
        // document.
        if let Some(warning) =
            crate::durability::bind_session(&cfg.durability, tools, &cfg.workspace_root, &record.id)
        {
            eprintln!("  {warning}");
        }
        Self {
            registry,
            record,
            name,
            durability: cfg.durability.clone(),
        }
    }

    /// The registry id — what executions link to and notifications carry.
    pub(crate) fn id(&self) -> &str {
        &self.record.id
    }

    /// The workspace's display name (notification titles).
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The headless one-shot → `/inbox` notification decision, shared by the
    /// pipeline and raw (`--no-pipeline`) paths so the two cannot drift: a
    /// failed run always lands a notification; a successful one only when it
    /// ran long enough (60s) that the user has plausibly looked away.
    pub(crate) fn one_shot_notification(
        &self,
        run_ok: bool,
        run_secs: u64,
        prompt: &str,
    ) -> Option<(String, String)> {
        let title = if !run_ok {
            format!("{}: run FAILED", self.name())
        } else if run_secs >= 60 {
            format!("{}: run finished ({run_secs}s)", self.name())
        } else {
            return None;
        };
        Some((title, crate::command_deck::prompt_line(prompt, 160)))
    }

    /// A new prompt is running: refresh the summary (and the title, if the
    /// session was announced before its first real prompt).
    pub(crate) fn update_prompt(&mut self, prompt: &str) {
        self.record.summary = crate::command_deck::prompt_line(prompt, 240);
        self.record.status = stella_store::SessionStatus::InProgress;
        self.record.title = format!(
            "{}: {}",
            self.name,
            crate::command_deck::prompt_line(prompt, 48)
        );
        let _ = self.registry.upsert(&self.record);
    }

    /// Between turns an interactive session waits on the human.
    pub(crate) fn needs_input(&mut self) {
        self.record.status = stella_store::SessionStatus::NeedsInput;
        let _ = self.registry.upsert(&self.record);
    }

    /// Terminal status, plus an optional persist-until-read inbox
    /// notification linked to this session — the headless → `/inbox` flow:
    /// a finished `stella run` surfaces in every deck's inbox, and `Enter`
    /// replays it.
    pub(crate) fn finish(&mut self, ok: bool, notify: Option<(String, String)>) {
        self.record.status = if ok {
            stella_store::SessionStatus::Complete
        } else {
            stella_store::SessionStatus::Error
        };
        let _ = self.registry.upsert(&self.record);
        // The headless counterpart of the deck's exit compaction. A one-shot
        // run writes fewer objects than a long deck session, but it is also the
        // shape that runs a hundred times in a loop from a script — which is
        // exactly how a workspace accumulates loose objects nobody is watching.
        self.durability.compact();
        if let Some((title, body)) = notify {
            let _ = stella_store::NotificationStore::open_default().push(
                &stella_store::Notification::new(title, body, self.record.id.clone())
                    .with_session_id(self.record.id.clone()),
            );
        }
    }
}
