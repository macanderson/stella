// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The execution context a tool runs in (#3284): the workspace root it has
//! always had, plus the event handle it never had.
//!
//! `tool.call.progress` was declared in `stella-core`'s bus names and emitted
//! nowhere, structurally: `Tool::execute(&self, input, root)` carried no way
//! to author an event, so only the registry itself could put anything on the
//! stream. [`ToolCtx`] is that channel — and #2694's journal discipline gets
//! its carrier: an infrastructure-mutating tool can now emit replayable
//! events the way file mutations already do.
//!
//! # The contract is the allowlist
//!
//! [`crate::contracts::contract_for`] resolves each tool's declared
//! `events` (#2716 §4), and [`ToolCtx::emit`] **drops** any name the
//! contract does not declare — a tool cannot improvise a vocabulary its
//! reviewers never saw. The drop is reported (`emit` returns whether the
//! event went out) but deliberately not an error: an event is telemetry, and
//! failing the call over it would let an observability bug break a working
//! tool.
//!
//! # Events never ride the output
//!
//! The loop detector keys on output bytes, so a timing or progress marker in
//! [`stella_protocol::tool::ToolOutput`] makes identical calls look distinct
//! and defeats it — the lesson that cost a bench run. Emission goes to the
//! hook bus, never into `content`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use stella_core::bus::HookBus;
use stella_core::workspace_scope::{ScopeDecision, SessionScope};

/// What a tool may touch while it runs: the workspace root for path
/// resolution, and the session's event channel scoped to this call's
/// declared vocabulary.
///
/// Owned rather than borrowed because it crosses an `async_trait` boundary
/// on every dispatch; the bus is a cheap shared-inner clone and the
/// allowlist is a handful of names.
pub struct ToolCtx {
    root: PathBuf,
    tool: String,
    bus: Option<HookBus>,
    allowed: Vec<String>,
    /// Where this session may write ([`stella_core::workspace_scope`]).
    ///
    /// Carried on the context rather than constructed per tool because it is
    /// one session-wide answer that several tools must give identically: a
    /// `write_file` refusal and a `bash` refusal that disagreed about the
    /// boundary would be worse than either alone. Defaults to the session
    /// root, so a context built before an operator widened anything confines
    /// to the workspace — the safe direction.
    scope: SessionScope,
}

impl ToolCtx {
    /// The context [`crate::registry::ToolRegistry::execute`] assembles per
    /// call: this tool's name, its contract-declared event names, and the
    /// session bus (when one is attached).
    #[must_use]
    pub fn new(
        root: PathBuf,
        tool: impl Into<String>,
        bus: Option<HookBus>,
        allowed: Vec<String>,
    ) -> Self {
        let scope = SessionScope::new(canonical_or_given(&root));
        Self {
            root,
            tool: tool.into(),
            bus,
            allowed,
            scope,
        }
    }

    /// The same context, confined to `scope` instead of to the session root
    /// alone — how an operator's `--allow-dir` / `stella.toml` widening
    /// reaches the tools.
    #[must_use]
    pub fn with_scope(mut self, scope: SessionScope) -> Self {
        self.scope = scope;
        self
    }

    /// A context with no event channel — for tests and for callers that
    /// execute a tool outside a registry. Every `emit` is a no-op that
    /// reports the drop.
    #[must_use]
    pub fn bare(root: PathBuf) -> Self {
        let scope = SessionScope::new(canonical_or_given(&root));
        Self {
            root,
            tool: String::new(),
            bus: None,
            allowed: Vec::new(),
            scope,
        }
    }

    /// The workspace root — exactly the `root` parameter `Tool::execute`
    /// carried before #3284. Tools must never read or write outside it
    /// without explicit opt-in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where this session may write, and what it may not read.
    #[must_use]
    pub fn scope(&self) -> &SessionScope {
        &self.scope
    }

    /// Resolve a model-supplied path for a **mutation**, returning the allowed
    /// root to open it under and its path relative to that root.
    ///
    /// This is the seam that makes more than one writable directory possible
    /// without teaching [`crate::rootfd::RootHandle`] about sets: the handle
    /// stays single-root and TOCTOU-safe, and this function decides *which*
    /// root it should be opened on. A bare relative path resolves against the
    /// session root, exactly as before; an absolute path — or one that climbs
    /// out with `..` — resolves against whichever allowed root contains it,
    /// and is refused when none does.
    ///
    /// The returned relative path is used only to walk the handle, so the
    /// descriptor walk still re-checks every component. This decides policy;
    /// `rootfd` closes the race.
    pub fn resolve_for_write(&self, path: &str) -> Result<(PathBuf, String), ScopeRefusal> {
        let absolute = self.absolutize(path);

        let decision = self.scope.may_write(&absolute);
        if !decision.is_allowed() {
            return Err(self.refusal(&absolute, decision));
        }

        // The most specific containing root wins, so a nested allowed
        // directory is opened as itself rather than reached through its
        // parent — the handle's confinement is then the tighter of the two.
        let normalized = lexically_normalize(&absolute);
        let mut best: Option<&PathBuf> = None;
        for root in self.scope.roots() {
            if normalized.starts_with(root)
                && best
                    .is_none_or(|current| root.components().count() > current.components().count())
            {
                best = Some(root);
            }
        }
        let Some(root) = best else {
            return Err(self.refusal(&absolute, ScopeDecision::OutsideScope));
        };
        let relative = normalized
            .strip_prefix(root)
            .map_err(|_| self.refusal(&absolute, ScopeDecision::OutsideScope))?;
        Ok((root.clone(), relative.to_string_lossy().into_owned()))
    }

    /// Resolve a path for a **read**, returning the allowed root that holds it
    /// and its path relative to that root.
    ///
    /// **Reads are not confined to the scope** — the machine is readable, and
    /// this function's job is only to pick a root that lets `rootfd` open the
    /// path safely. Three cases, in order:
    ///
    /// 1. inside a writable root — open that root, so a `--allow-dir`
    ///    directory is readable as well as writable (without this the two
    ///    disagreed, and an agent could write a file it was then told did not
    ///    exist);
    /// 2. inside the session root — open the session root, exactly as before;
    /// 3. anywhere else on the machine — open the file's own parent
    ///    directory. `rootfd` still walks with `O_NOFOLLOW` from a held
    ///    descriptor, so the leaf cannot be swapped mid-open; the root is
    ///    simply narrower than a whole tree because there is no tree to
    ///    confine to.
    ///
    /// `None` only for a path with no parent at all — a bare filesystem
    /// root, which is a directory and not readable as a file anyway.
    pub fn resolve_for_read(&self, path: &str) -> Option<(PathBuf, String)> {
        let absolute = self.absolutize(path);
        let normalized = lexically_normalize(&absolute);
        let mut best: Option<&PathBuf> = None;
        for root in self.scope.roots() {
            if normalized.starts_with(root)
                && best
                    .is_none_or(|current| root.components().count() > current.components().count())
            {
                best = Some(root);
            }
        }
        if let Some(root) = best
            && let Ok(relative) = normalized.strip_prefix(root)
            && !relative.as_os_str().is_empty()
        {
            return Some((root.clone(), relative.to_string_lossy().into_owned()));
        }
        let parent = normalized.parent()?;
        let leaf = normalized.file_name()?;
        Some((parent.to_path_buf(), leaf.to_string_lossy().into_owned()))
    }

    /// Whether a model-supplied path may be written, as a refusal string when
    /// it may not — the shape a caller that is not opening the path itself
    /// needs (the shell audit in [`crate::bash`]).
    pub fn refuse_write(&self, path: &str) -> Option<String> {
        let absolute = self.absolutize(path);
        let decision = self.scope.may_write(&absolute);
        (!decision.is_allowed()).then(|| self.scope_refusal(&absolute, decision))
    }

    /// Whether a model-supplied path may be read, as a refusal string when it
    /// may not. Only worktrees are ever refused — see
    /// [`stella_core::workspace_scope`] on why reads are otherwise open.
    pub fn refuse_read(&self, path: &str) -> Option<String> {
        let absolute = self.absolutize(path);
        let decision = self.scope.may_read(&absolute);
        (!decision.is_allowed()).then(|| self.scope_refusal(&absolute, decision))
    }

    /// A model-supplied path as one absolute, **canonical-prefix** path, ready
    /// for the scope to decide about.
    ///
    /// Two steps, and the second is the one that is easy to skip and wrong to
    /// skip. A relative path joins the session root. Then the deepest part of
    /// the result that actually exists is canonicalized, because a scope's
    /// roots are canonical and a path that is not cannot be compared to them:
    /// on macOS `/var` is a symlink to `/private/var`, so an allowed directory
    /// handed in as `/var/…` and stored as `/private/var/…` would never match
    /// itself. The non-existent tail (the file about to be created) is
    /// re-appended lexically — it cannot be canonicalized because it is not
    /// there yet.
    ///
    /// **The final component is never resolved.** Canonicalizing it would
    /// follow a symlink, and `delete_file`'s whole contract is that it unlinks
    /// the *link* and spares the target — resolving the leaf here would have
    /// silently turned every symlink deletion into a deletion of the real
    /// file. Only the directory part is canonicalized, which is all the scope
    /// decision needs: a leaf cannot move a path into a different directory.
    ///
    /// This is deliberately I/O, which is why it lives here and not in
    /// `stella_core::workspace_scope` (invariant #2). It also does not close
    /// any race — `rootfd`'s descriptor walk does that; this only makes the
    /// *policy* question answerable.
    fn absolutize(&self, path: &str) -> PathBuf {
        let requested = Path::new(path);
        let joined = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        let joined = lexically_normalize(&joined);

        let (parent, leaf) = match (joined.parent(), joined.file_name()) {
            (Some(parent), Some(leaf)) => (parent, leaf),
            // A path with no parent is a filesystem root; there is no leaf to
            // protect and nothing to canonicalize toward.
            _ => return joined,
        };

        let mut existing = parent;
        let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
        loop {
            if let Ok(canonical) = existing.canonicalize() {
                let mut resolved = canonical;
                for part in missing.iter().rev() {
                    resolved.push(part);
                }
                resolved.push(leaf);
                return resolved;
            }
            let Some(next) = existing.parent() else {
                // Nothing on the path exists; the lexical form is the best
                // answer available, and the scope will judge it as such.
                return joined;
            };
            if let Some(name) = existing.file_name() {
                missing.push(name);
            }
            existing = next;
        }
    }

    /// The typed refusal for a path the scope rejected.
    fn refusal(&self, path: &Path, decision: ScopeDecision) -> ScopeRefusal {
        ScopeRefusal {
            decision,
            path: path.to_path_buf(),
            message: self.scope_refusal(path, decision),
        }
    }

    /// The refusal a tool returns for a path `scope` rejected, phrased for the
    /// model rather than for a log.
    ///
    /// One spelling, shared by every tool that confines, so `write_file` and
    /// `bash` cannot describe the same boundary two different ways. Each
    /// refusal names the remedy the *agent* can act on, never one only the
    /// user can: "ask the user to widen it" is a dead end mid-turn, while
    /// "work inside these directories" is something the next tool call can do.
    #[must_use]
    pub fn scope_refusal(&self, path: &Path, decision: ScopeDecision) -> String {
        match decision {
            ScopeDecision::Allowed => String::new(),
            // The two shapes the same denial takes, phrased for whichever
            // session hit it. Both name the remedy the AGENT can perform;
            // "ask the user to widen it" is a dead end mid-turn.
            ScopeDecision::DeniedOrigin if self.scope.in_worktree() => format!(
                "`{}` is inside `{}`, the project this worktree was cut from. \
                 A worktree is a separate checkout of that project at another \
                 revision, so reading the original tree would answer your question \
                 about the wrong copy of the file you are editing. Everything else on \
                 this machine is readable; that one tree is not, until the session \
                 leaves this worktree. Work inside `{}`.",
                path.display(),
                self.scope.origin().display(),
                self.scope.workspace().display()
            ),
            ScopeDecision::DeniedOrigin => format!(
                "`{}` is inside `{}`, which holds other sessions' git worktrees. \
                 Those are separate checkouts of this repository at other revisions: \
                 reading one returns a different branch's version of the file you are \
                 working on, and writing one changes work this session does not own. \
                 Work in the session root instead.",
                path.display(),
                stella_core::workspace_scope::WORKTREES_SUBPATH
            ),
            ScopeDecision::OutsideScope => {
                let roots = self
                    .scope
                    .roots()
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                // Naming `$STELLA_SCRATCH` matters more than naming the path.
                // Measured on `financial-document-processor`: refused at
                // `/tmp/apt.log`, the agent read "a scratch directory" off the
                // roots list, spent a whole turn on `get_environment` to learn
                // its absolute path, and only then retried — two steps to
                // rediscover a variable already exported into its shell. The
                // earlier shape of this failure was worse: with no scratch
                // named at all, an agent fell back to writing build artifacts
                // into the graded workspace.
                format!(
                    "`{}` is outside this session's writable directories ({roots}). \
                     Reading anywhere is fine; creating, editing, deleting or running \
                     a script outside those directories is not. For a working file \
                     that is not a deliverable, redirect to $STELLA_SCRATCH — it is \
                     already exported into your shell, so `> $STELLA_SCRATCH/out.log` \
                     works with no lookup, and nothing there lands in this turn's diff.",
                    path.display()
                )
            }
        }
    }

    /// Emit one named event onto the session bus, if this tool's contract
    /// declares the name.
    ///
    /// The payload must be a JSON object; a `"tool"` field naming the
    /// emitter is stamped in so an observer never has to guess attribution.
    /// Returns whether the event actually went out — `false` when the name
    /// is undeclared (dropped: the contract is the allowlist), when no bus
    /// is attached, or when the payload is not an object.
    pub fn emit(&self, event: &str, payload: Value) -> bool {
        let Some(bus) = &self.bus else {
            return false;
        };
        if !self.allowed.iter().any(|name| name == event) {
            return false;
        }
        let Value::Object(mut fields) = payload else {
            return false;
        };
        fields.insert("tool".into(), Value::String(self.tool.clone()));
        bus.emit_named(event, Value::Object(fields));
        true
    }

    /// Emit `tool.call.progress` — sugar for the one event most tools that
    /// emit anything will emit.
    pub fn progress(&self, payload: Value) -> bool {
        self.emit(stella_core::bus::names::TOOL_CALL_PROGRESS, payload)
    }
}

/// Why a path was refused, carrying the decision a caller can branch on
/// (invariant #5).
///
/// The message is prose for the model; [`Self::decision`] is the fact. They
/// are separate fields because a caller wanting to tell "outside the scope"
/// from "that is another session's worktree" must not have to match on the
/// prose — the two want different handling (widen the scope versus never), and
/// the wording is free to improve without breaking anyone.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ScopeRefusal {
    /// Which rule refused the path.
    pub decision: ScopeDecision,
    /// The absolute path as the scope saw it, after resolution.
    pub path: PathBuf,
    /// The refusal as the model reads it.
    message: String,
}

/// `root` canonicalized, or `root` unchanged when it cannot be.
///
/// A scope's roots must be canonical or nothing can be compared against them
/// (see [`ToolCtx::absolutize`]). A root that does not exist yet is kept as
/// given rather than dropped: refusing to build a scope because the workspace
/// is missing would fail the session for a reason the session cannot fix, and
/// a non-canonical root still confines — it just confines a path spelled the
/// same way.
fn canonical_or_given(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Lexical `.`/`..` collapse — the same normalization
/// [`stella_core::workspace_scope`] applies before deciding, repeated here so
/// the path handed to `strip_prefix` is the one the decision was made about.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    fn recording_bus() -> (HookBus, Seen) {
        let bus = HookBus::new("test-session");
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let sink = seen.clone();
        bus.on("tool.call.*", move |event| {
            sink.lock()
                .unwrap()
                .push((event.name.clone(), event.payload.clone()));
            Ok(())
        })
        .detach();
        (bus, seen)
    }

    /// The #3284 witness's other half: an undeclared event name is dropped
    /// rather than forwarded — the contract is the allowlist.
    #[test]
    fn an_undeclared_event_name_is_dropped() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "delegate",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(!ctx.emit("tool.call.invented", json!({ "stage": "x" })));
        assert!(seen.lock().unwrap().is_empty(), "nothing may reach the bus");
    }

    #[test]
    fn a_declared_event_reaches_the_bus_stamped_with_its_tool() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "delegate",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(ctx.progress(json!({ "stage": "dispatched" })));
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "tool.call.progress");
        assert_eq!(events[0].1["tool"], "delegate");
        assert_eq!(events[0].1["stage"], "dispatched");
    }

    #[test]
    fn a_bare_context_drops_everything_quietly() {
        let ctx = ToolCtx::bare(PathBuf::from("."));
        assert!(!ctx.progress(json!({ "stage": "x" })));
    }

    /// A non-object payload cannot carry the `tool` attribution stamp, so it
    /// is dropped rather than emitted anonymous.
    #[test]
    fn a_non_object_payload_is_dropped() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "delegate",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(!ctx.progress(json!("just a string")));
        assert!(seen.lock().unwrap().is_empty());
    }
}
