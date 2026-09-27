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
    /// Whether a prompt has named the session yet. Only the first prompt
    /// names it, so a later prompt never renames a session under the user.
    named: bool,
    /// This session's durable record, carried so [`Self::finish`] can compact
    /// it. Cloning the handle rather than re-opening the record keeps the
    /// compaction pointed at the session that was actually bound, whatever the
    /// driver did in between.
    durability: crate::durability::SessionDurability,
}

impl SessionPresence {
    /// Announce the session (status In Progress), named from the prompt or
    /// goal that started it, and bind this session's durability. An
    /// interactive session passes `None` and takes its name from its first
    /// prompt instead.
    ///
    /// The binding is done HERE, rather than left to each headless driver,
    /// because this is the moment the session acquires the identity durability
    /// is keyed on. A driver that announced without binding would run a whole
    /// session whose turns checkpoint nowhere — and the failure would be
    /// silent, because an unbound sink is indistinguishable from a session
    /// that simply never crashed.
    pub(crate) fn announce(cfg: &Config, prompt: Option<&str>) -> Self {
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
        let record = crate::daemon::supervised_id()
            .and_then(|id| registry.get(&id))
            .unwrap_or_else(|| {
                stella_store::SessionRecord::new(
                    cfg.workspace_root.display().to_string(),
                    name.clone(),
                )
            });
        let presence = Self::begin(registry, record, name, cfg.durability.clone(), prompt);
        // stderr, not stdout: `--output-format json` owns stdout, and a
        // durability advisory must never land inside a machine-readable
        // document.
        if let Some(warning) = crate::durability::bind_session(
            &cfg.durability,
            &cfg.workspace_root,
            &presence.record.id,
        ) {
            eprintln!("  {warning}");
        }
        presence
    }

    /// Register `record`, named from `prompt` when there is one. This is
    /// [`Self::announce`] without the default registry, the supervisor
    /// lookup, and the durability binding, so a test can hand it a registry
    /// in a temporary directory.
    fn begin(
        registry: stella_store::SessionRegistry,
        record: stella_store::SessionRecord,
        name: String,
        durability: crate::durability::SessionDurability,
        prompt: Option<&str>,
    ) -> Self {
        let mut presence = Self {
            registry,
            record,
            name,
            named: false,
            durability,
        };
        match prompt {
            Some(prompt) => presence.update_prompt(prompt),
            None => {
                let _ = presence.registry.upsert(&presence.record);
            }
        }
        presence
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
    /// run that did not complete always lands a notification, worded by how
    /// it actually ended — a policy stop is deliberate, and "FAILED" for it
    /// was the same dishonesty the registry status carried (#1826); a
    /// successful one notifies only when it ran long enough (60s) that the
    /// user has plausibly looked away.
    pub(crate) fn one_shot_notification(
        &self,
        status: stella_store::SessionStatus,
        run_secs: u64,
        prompt: &str,
    ) -> Option<(String, String)> {
        let title = match status {
            stella_store::SessionStatus::Complete if run_secs >= 60 => {
                format!("{}: run finished ({run_secs}s)", self.name())
            }
            stella_store::SessionStatus::Complete => return None,
            stella_store::SessionStatus::Stopped => {
                format!("{}: run stopped by policy", self.name())
            }
            stella_store::SessionStatus::Cancelled => {
                format!("{}: run cancelled", self.name())
            }
            // Anything else a terminal write carries is a genuine failure.
            _ => format!("{}: run FAILED", self.name()),
        };
        Some((title, crate::command_deck::prompt_line(prompt, 160)))
    }

    /// A new prompt is running: refresh the summary and the status. The first
    /// prompt also names the session. A later prompt leaves the name alone.
    pub(crate) fn update_prompt(&mut self, prompt: &str) {
        self.record.summary = crate::command_deck::prompt_line(prompt, 240);
        self.record.status = stella_store::SessionStatus::InProgress;
        if !self.named {
            self.record.title = crate::session_name::session_name(prompt);
            self.named = true;
        }
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
    ///
    /// `status` is the caller's own terminal answer projected by
    /// [`crate::daemon::outcome_status`] (or `super::outcome`'s pipeline
    /// projection) — never a bool. The bool collapsed a deliberate stop into
    /// `Error`, and on an unsupervised headless run no later supervised write
    /// corrected it, so the SESSIONS overlay painted a policy stop as a crash
    /// (#1826, the presence half of #1653).
    pub(crate) fn finish(
        &mut self,
        status: stella_store::SessionStatus,
        notify: Option<(String, String)>,
    ) {
        self.record.status = status;
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

#[cfg(test)]
mod tests {
    use super::SessionPresence;

    /// The first prompt names the session and a later prompt keeps that
    /// name. The old code retitled on every prompt, so an interactive
    /// session wore the name of whatever it was asked last.
    #[test]
    fn only_the_first_prompt_names_the_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = stella_store::SessionRegistry::open(dir.path());
        let record = stella_store::SessionRecord::new("/w/stella", "stella");
        let mut presence = SessionPresence::begin(
            registry.clone(),
            record,
            "stella".to_string(),
            crate::durability::SessionDurability::default(),
            None,
        );
        let id = presence.id().to_string();
        assert_eq!(registry.get(&id).expect("registered").title, "stella");

        presence.update_prompt("https://github.com/macanderson/stella/pull/123 fix conflicts");
        assert_eq!(
            registry.get(&id).expect("registered").title,
            "Fix conflicts on PR 123"
        );

        presence.update_prompt("now run the tests again");
        let stored = registry.get(&id).expect("registered");
        assert_eq!(stored.title, "Fix conflicts on PR 123");
        assert_eq!(stored.summary, "now run the tests again");
    }

    /// A headless run is named from its prompt the moment it is announced.
    #[test]
    fn a_headless_run_is_named_from_its_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = stella_store::SessionRegistry::open(dir.path());
        let record = stella_store::SessionRecord::new("/w/stella", "stella");
        let presence = SessionPresence::begin(
            registry.clone(),
            record,
            "stella".to_string(),
            crate::durability::SessionDurability::default(),
            Some("fix the flaky parser test. Then push."),
        );
        let stored = registry.get(presence.id()).expect("registered");
        assert_eq!(stored.title, "Fix the flaky parser test");
    }
}
