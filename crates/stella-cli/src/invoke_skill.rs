//! The `invoke_skill` session tool — skills as executable procedures (#2682).
//!
//! [`crate::discovery::DiscoveryToolSet`] mounts this beside `skill_search`:
//! search finds a skill, this runs it. The tool input carries only the slug
//! and its arguments (invariant #9 — a parameter scopes, never selects);
//! **how** the invocation behaves is declared by the skill's own frontmatter,
//! parsed by `stella_core::skills::invoke`:
//!
//! - **Inline** (default): the body — with `$ARGUMENTS` substituted — is
//!   queued on [`SkillInjections`] and lands in the transcript as a tail
//!   user-role message under `SKILL_INVOCATION_PREFIX`, through the engine's
//!   existing step-boundary steering seam
//!   (`stella_core::ports::TurnSteering`). A driver that wires the queue into
//!   its turn calls [`SkillInjections::arm`]; on an unwired surface the tool
//!   result itself carries the expansion, so the instructions are never
//!   silently lost — they just ride the tool-result channel instead.
//! - **`context: fork`**: the skill runs in a sub-agent through the session's
//!   existing [`SubAgentDispatcher`] and only its result crosses back; the
//!   full body never enters the parent transcript. `effort:` overrides the
//!   child's reasoning effort; an `allowed-tools` grant is resolved against
//!   the session surface and enforced structurally by
//!   `stella_core::ports::GrantedTools` inside the child.
//!
//! # The allowed-tools layer
//!
//! An inline skill's grant arms a narrowing layer for the rest of the turn:
//! [`InvokeSkillTools::allows`] answers from the intersection of every live
//! grant, and the mounting `DiscoveryToolSet` consults it on both `schemas`
//! and `execute`. The operator's own policy is enforced *below* this layer by
//! `PolicyToolSet`, so the effective surface is structurally
//! `grant ∩ operator` — a skill can narrow or select within the operator
//! surface but can never re-enable a tool the operator or an org-managed
//! scope denied (`stella_tools::skill_grant` carries the property tests).
//! Teardown is structural: the layers (and the matching
//! `ActiveSkillInvocations` spans) live in this per-turn tool stack and pop
//! when it drops.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;
use stella_core::ports::{ToolExecutor, TurnSteering};
use stella_core::skills::Skill;
use stella_core::skills::invoke::{
    ActiveSkillInvocations, DirectiveDiagnostic, InvokeDirectives, SkillInvocationMode, SkillSpan,
    parse_invoke_directives, render_invocation_message, substitute_arguments,
};
use stella_core::subagent::{SubAgentDispatcher, SubAgentOutcome, SubAgentSpec};
use stella_protocol::{ToolOutput, ToolSchema};
use stella_tools::policy::ToolPolicy;
use stella_tools::skill_grant::{grant_policy, resolve_grant};

pub const INVOKE_SKILL: &str = "invoke_skill";

/// Ceiling on a forked skill's model calls — the `task` tool's bound, for the
/// same reason: enough for a real procedure, too little for a runaway.
const FORK_MAX_STEPS: usize = 16;
/// Ceiling on the characters a forked skill hands back (the context-economy
/// cap; matches the `task` tool).
const FORK_REPORT_CHARS: usize = 8_000;

/// Inline expansions awaiting injection as tail user-role messages.
///
/// Implements [`TurnSteering`] so a driver wires it with the seam that
/// already exists for mid-turn user messages: publish it as (part of) the
/// turn's steering and the engine injects each expansion at the next step
/// boundary, before compaction and before the model call. **Call
/// [`Self::arm`] when — and only when — a turn actually drains this
/// handle**: an unarmed queue makes the tool fall back to carrying the
/// expansion in its own result, so an unwired surface degrades to visible
/// instructions rather than to silence.
#[derive(Clone, Default)]
pub struct SkillInjections {
    inner: Arc<InjectionsInner>,
}

#[derive(Default)]
struct InjectionsInner {
    queue: Mutex<Vec<String>>,
    armed: AtomicBool,
}

impl SkillInjections {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that a turn drains this queue (see the type docs).
    pub fn arm(&self) {
        self.inner.armed.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.inner.armed.load(Ordering::SeqCst)
    }

    fn push(&self, expansion: String) {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(expansion);
    }
}

impl TurnSteering for SkillInjections {
    fn drain_steering(&self) -> Vec<String> {
        std::mem::take(&mut *self.inner.queue.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Never stops a turn — this tap only ever injects.
    fn soft_stop_requested(&self) -> bool {
        false
    }
}

/// One live inline invocation: its span (for the #2685 compaction seam) and
/// its optional tool grant. Dropping the struct — with the per-turn tool
/// stack that owns it — is the structural teardown for both.
struct InlineLayer {
    slug: String,
    grant: Option<ToolPolicy>,
    _span: SkillSpan,
}

/// The `invoke_skill` implementation `DiscoveryToolSet` mounts. One per tool
/// stack, i.e. per turn.
pub struct InvokeSkillTools {
    workspace_root: PathBuf,
    injections: SkillInjections,
    active: ActiveSkillInvocations,
    layers: Mutex<Vec<InlineLayer>>,
    dispatcher: RwLock<Option<Arc<dyn SubAgentDispatcher>>>,
}

impl InvokeSkillTools {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            injections: SkillInjections::new(),
            active: ActiveSkillInvocations::new(),
            layers: Mutex::new(Vec::new()),
            dispatcher: RwLock::new(None),
        }
    }

    /// Attach the session's sub-agent runner, enabling `context: fork`
    /// skills. Without it a fork-mode invocation is refused with a named
    /// error — the `task` tool's unfilled-slot posture.
    pub fn set_dispatcher(&self, dispatcher: Arc<dyn SubAgentDispatcher>) {
        *self.dispatcher.write().unwrap_or_else(|p| p.into_inner()) = Some(dispatcher);
    }

    /// The injection queue, for the driver that wires it into its turn's
    /// steering (and then arms it).
    #[must_use]
    pub fn injections(&self) -> SkillInjections {
        self.injections.clone()
    }

    /// The live-invocation tracking handle — the overflow summarizer's
    /// restoration seam (#2685): `DiscoveryToolSet` surfaces it to the
    /// engine as `ToolExecutor::active_skill_slugs`.
    #[must_use]
    pub fn active_invocations(&self) -> ActiveSkillInvocations {
        self.active.clone()
    }

    /// Whether every live allowed-tools grant permits `name`. `true` when no
    /// grant is armed. The operator policy is enforced by the stack below,
    /// so this is the grant half of `grant ∩ operator`.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        self.layers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .all(|layer| layer.grant.as_ref().is_none_or(|grant| grant.allows(name)))
    }

    /// The slug of the innermost live grant that denies `name`, for the
    /// refusal message.
    fn denying_slug(&self, name: &str) -> Option<String> {
        self.layers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .rev()
            .find(|layer| {
                layer
                    .grant
                    .as_ref()
                    .is_some_and(|grant| !grant.allows(name))
            })
            .map(|layer| layer.slug.clone())
    }

    /// The refusal for a tool a live skill grant withholds, or `None` when
    /// `name` may proceed.
    #[must_use]
    pub fn grant_refusal(&self, name: &str) -> Option<ToolOutput> {
        let slug = self.denying_slug(name)?;
        Some(ToolOutput::error(format!(
            "`{name}` is unavailable while skill `{slug}` is active — its allowed-tools \
                 grant scopes this turn to a narrower tool set"
        )))
    }

    /// Drop every schema a live grant withholds.
    pub fn filter_schemas(&self, schemas: &mut Vec<ToolSchema>) {
        schemas.retain(|schema| self.allows(&schema.name));
    }

    pub fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: INVOKE_SKILL.into(),
            description: "Invoke an installed skill by name (find names with skill_search). \
                 The skill's own frontmatter decides how it runs: inline — its full \
                 instructions are appended to this conversation with $ARGUMENTS substituted \
                 — or fork, where a sub-agent executes it and only the result returns. Pass \
                 arguments when the skill expects them."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "The skill's name exactly as skill_search lists it."
                    },
                    "arguments": {
                        "description": "Substituted for $ARGUMENTS in the skill body: a \
                             string, or a flat object rendered as key=value pairs.",
                        "anyOf": [
                            { "type": "string" },
                            { "type": "object" }
                        ]
                    }
                },
                "required": ["skill"],
                "additionalProperties": false
            }),
            // Not read-only: an inline invocation mutates the turn's
            // transcript and arms a tool grant; a fork spends money.
            read_only: false,
            speculation_safe: false,
        }
    }

    /// Execute one invocation. `inner` is the complete tool surface below
    /// the discovery layer — what grants resolve against; `include_workspace`
    /// is the mounting layer's project-prompt authority.
    pub async fn execute(
        &self,
        input: &Value,
        inner: &dyn ToolExecutor,
        include_workspace: bool,
    ) -> ToolOutput {
        if crate::settings::filesystem_settings_disabled() {
            return ToolOutput::error("invoke_skill is disabled by benchmark filesystem isolation");
        }
        let Some(name) = input.get("skill").and_then(Value::as_str).map(str::trim) else {
            return ToolOutput::error(
                "invoke_skill: missing required string field `skill` — find names \
                     with skill_search",
            );
        };
        if name.is_empty() {
            return ToolOutput::error("invoke_skill: `skill` is empty");
        }
        let arguments = match render_arguments(input.get("arguments")) {
            Ok(arguments) => arguments,
            Err(message) => {
                return ToolOutput::error(message);
            }
        };

        // Fresh load, same as skill_search: a skill installed seconds ago is
        // already invocable. The raw file is re-read for its frontmatter —
        // the loader keeps only name/description/domains/body.
        let root = self.workspace_root.clone();
        let wanted = name.to_string();
        let loaded = tokio::task::spawn_blocking(move || {
            let skills =
                crate::memory::load_workspace_skills_with_authority(&root, include_workspace)
                    .skills;
            let found = skills.iter().position(|s| s.name == wanted).or_else(|| {
                skills
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(&wanted))
            });
            found.map(|idx| {
                let skill = skills[idx].clone();
                let directives = std::fs::read_to_string(&skill.source_path)
                    .map(|raw| parse_invoke_directives(&raw))
                    .unwrap_or_default();
                (skill, directives)
            })
        })
        .await
        .unwrap_or_default();

        let Some((skill, directives)) = loaded else {
            return ToolOutput::error(format!(
                "no installed skill named `{name}` — use skill_search to find the \
                     right name, or search_skills for the public registry"
            ));
        };

        match directives.mode {
            SkillInvocationMode::Inline => self.invoke_inline(&skill, &directives, &arguments),
            SkillInvocationMode::Fork => {
                self.invoke_fork(&skill, &directives, &arguments, inner)
                    .await
            }
        }
    }

    /// Inline mode: expand under the marker, queue (or carry) the expansion,
    /// and arm the grant/span layer for the rest of the turn.
    fn invoke_inline(
        &self,
        skill: &Skill,
        directives: &InvokeDirectives,
        arguments: &str,
    ) -> ToolOutput {
        let body = substitute_arguments(&skill.body, arguments);
        let expansion = render_invocation_message(&skill.name, &body);

        let mut notes = directive_notes(directives);
        if directives.model.is_some() || directives.effort.is_some() {
            notes.push(
                "model:/effort: overrides apply to fork mode and were ignored for this \
                 inline invocation"
                    .to_string(),
            );
        }
        if let Some(allowed) = &directives.allowed_tools {
            self.layers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(InlineLayer {
                    slug: skill.name.clone(),
                    grant: Some(grant_policy(allowed)),
                    _span: self.active.begin(&skill.name),
                });
            notes.push(format!(
                "allowed-tools grant active for the rest of this turn: {}",
                allowed.join(", ")
            ));
        } else {
            self.layers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(InlineLayer {
                    slug: skill.name.clone(),
                    grant: None,
                    _span: self.active.begin(&skill.name),
                });
        }

        let notes = render_notes(&notes);
        if self.injections.is_armed() {
            self.injections.push(expansion);
            ToolOutput::Ok {
                content: format!(
                    "skill `{}` invoked — its instructions arrive as the next user \
                     message; follow them{notes}",
                    skill.name
                ),
            }
        } else {
            // No driver drains the queue on this surface: the expansion rides
            // the tool result instead of vanishing into an undrained queue.
            ToolOutput::Ok {
                content: format!("{expansion}{notes}"),
            }
        }
    }

    /// Fork mode: run the skill in a bounded child through the session's
    /// sub-agent runner; only the result crosses back.
    async fn invoke_fork(
        &self,
        skill: &Skill,
        directives: &InvokeDirectives,
        arguments: &str,
        inner: &dyn ToolExecutor,
    ) -> ToolOutput {
        let dispatcher = {
            let slot = self.dispatcher.read().unwrap_or_else(|p| p.into_inner());
            slot.clone()
        };
        let Some(dispatcher) = dispatcher else {
            return ToolOutput::error(format!(
                "skill `{}` is fork-mode (context: fork) but no sub-agent runner is \
                     attached in this session — do the work directly, using the skill's \
                     guidance from skill_search",
                skill.name
            ));
        };

        let mut notes = directive_notes(directives);
        if let Some(model) = &directives.model {
            // Honoring a per-skill model needs a provider resolved per spawn,
            // which the session dispatcher does not carry yet — declared, not
            // silent (#2682 follow-up).
            notes.push(format!(
                "model: {model} is not yet honored for forked skills — ran on the \
                 session's sub-agent provider"
            ));
        }
        let allowed_tools = directives.allowed_tools.as_ref().map(|allowed| {
            let advertised: Vec<String> = inner
                .schemas()
                .into_iter()
                .map(|schema| schema.name)
                .collect();
            resolve_grant(allowed, &advertised)
        });

        let body = substitute_arguments(&skill.body, arguments);
        let instruction = if arguments.is_empty() {
            "Carry out your instructions and report the result.".to_string()
        } else {
            format!("Carry out your instructions for: {arguments}")
        };
        let spec = SubAgentSpec {
            agent_id: format!("skill-{}", slug(&skill.name)),
            // The body IS the child's charter; the parent transcript never
            // sees it.
            system_prompt: Some(body),
            instruction,
            max_steps: FORK_MAX_STEPS,
            max_report_chars: FORK_REPORT_CHARS,
            // Mutation from a forked skill would need the parent's review
            // gates around it — same posture as the `task` tool.
            write_access: false,
            depth: 1,
            effort: directives.effort,
            allowed_tools,
            ..SubAgentSpec::default()
        };

        let outcome = dispatcher.dispatch(spec).await;
        render_fork_outcome(&skill.name, &outcome, &render_notes(&notes))
    }
}

/// Render the model's `arguments` input to the substitution string: a string
/// verbatim, an object as sorted `key=value` pairs, absent as empty.
fn render_arguments(value: Option<&Value>) -> Result<String, String> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.trim().to_string()),
        Some(Value::Object(map)) => {
            let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
            pairs.sort_by_key(|(key, _)| key.as_str());
            Ok(pairs
                .into_iter()
                .map(|(key, value)| match value {
                    Value::String(s) => format!("{key}={s}"),
                    other => format!("{key}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" "))
        }
        Some(other) => Err(format!(
            "invoke_skill: `arguments` must be a string or a flat object, got {other}"
        )),
    }
}

/// The degraded-directive notes a caller should see instead of a silent drop.
fn directive_notes(directives: &InvokeDirectives) -> Vec<String> {
    directives
        .diagnostics
        .iter()
        .map(|diagnostic| match diagnostic {
            DirectiveDiagnostic::UnknownContext { value } => format!(
                "unknown context: `{value}` in the skill's frontmatter — ran inline (the \
                 default)"
            ),
            DirectiveDiagnostic::UnknownEffort { value } => {
                format!("unknown effort: `{value}` in the skill's frontmatter — override dropped")
            }
        })
        .collect()
}

/// Notes block appended to a tool result — empty string when there are none.
fn render_notes(notes: &[String]) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!("\n[{}]", notes.join("; "))
    }
}

/// A forked skill's outcome as a model-visible result: the child's report
/// only — mirroring the `task` tool's salvage-don't-bury shape.
fn render_fork_outcome(skill: &str, outcome: &SubAgentOutcome, notes: &str) -> ToolOutput {
    match outcome {
        SubAgentOutcome::Completed(report) => ToolOutput::Ok {
            content: if report.truncated {
                format!(
                    "{}\n\n[skill `{skill}` result truncated at {FORK_REPORT_CHARS} \
                     characters]{notes}",
                    report.summary
                )
            } else {
                format!("{}{notes}", report.summary)
            },
        },
        SubAgentOutcome::Incomplete { report, reason } if !report.summary.is_empty() => {
            ToolOutput::Ok {
                content: format!(
                    "{}\n\n[partial: skill `{skill}` stopped before finishing — {reason}]{notes}",
                    report.summary
                ),
            }
        }
        SubAgentOutcome::Incomplete { reason, .. } => ToolOutput::error(format!(
            "skill `{skill}` stopped before producing anything: {reason}"
        )),
        SubAgentOutcome::Refused { reason } => {
            ToolOutput::error(format!("skill `{skill}` was not started: {reason}"))
        }
    }
}

/// A short, stable, attribution-safe id from the skill name (the `task`
/// tool's slug shape).
fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use stella_core::skills::invoke::SKILL_INVOCATION_PREFIX;

    /// A minimal inner surface for grants to resolve against.
    struct FakeInner;

    #[async_trait]
    impl ToolExecutor for FakeInner {
        fn schemas(&self) -> Vec<ToolSchema> {
            ["read_file", "grep", "bash", "write_file"]
                .into_iter()
                .map(|name| ToolSchema {
                    name: name.into(),
                    description: String::new(),
                    input_schema: serde_json::json!({"type": "object"}),
                    read_only: name == "read_file" || name == "grep",
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: format!("inner ran {name}"),
            }
        }
    }

    /// Records the spec it was handed; returns a scripted outcome.
    struct RecordingDispatcher {
        seen: Mutex<Option<SubAgentSpec>>,
        outcome: SubAgentOutcome,
    }

    impl RecordingDispatcher {
        fn completed(summary: &str) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(None),
                outcome: SubAgentOutcome::Completed(stella_core::subagent::SubAgentReport {
                    summary: summary.to_string(),
                    truncated: false,
                    cost_usd: 0.01,
                    steps: 2,
                    absorbed_messages: 4,
                }),
            })
        }
    }

    #[async_trait]
    impl SubAgentDispatcher for RecordingDispatcher {
        async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
            *self.seen.lock().unwrap() = Some(spec);
            self.outcome.clone()
        }
    }

    /// Write a skill in the ecosystem layout under the workspace root.
    fn write_skill(root: &std::path::Path, slug: &str, frontmatter_extra: &str, body: &str) {
        let dir = root.join(".stella/skills").join(slug);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {slug}\ndescription: a test skill\n{frontmatter_extra}---\n{body}\n"
            ),
        )
        .expect("skill file");
    }

    /// Run one invocation against a workspace with HOME redirected to an
    /// empty tempdir (same discipline as discovery.rs's skill_search tests):
    /// sync so the env lock spans the whole mutation window.
    fn invoke_with_isolated_home(
        ws: &std::path::Path,
        tools: &InvokeSkillTools,
        input: Value,
    ) -> ToolOutput {
        let _lock = crate::test_env::lock();
        let _restore = crate::test_env::EnvRestore::capture(&["HOME"]);
        let home = tempfile::tempdir().expect("home");
        unsafe { std::env::set_var("HOME", home.path()) };
        let _ = ws;
        let inner = FakeInner;
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(tools.execute(&input, &inner, true))
    }

    /// #2682 witness (a): invoking a skill with arguments queues the full
    /// body — `$ARGUMENTS` substituted — as a tail user-role message under
    /// the invocation marker. (The engine side of the seam — drained
    /// steering entering the transcript as user messages — is pinned by
    /// `stella-core`'s driver steering tests.)
    #[test]
    fn inline_invocation_queues_the_substituted_body_under_the_marker() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(
            ws.path(),
            "release-notes",
            "",
            "Write release notes for $ARGUMENTS. Lead with user impact.",
        );
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let injections = tools.injections();
        injections.arm();

        let out = invoke_with_isolated_home(
            ws.path(),
            &tools,
            serde_json::json!({"skill": "release-notes", "arguments": "v2.4.0"}),
        );
        assert!(!out.is_error(), "invocation should succeed, got {out:?}");

        let queued = injections.drain_steering();
        assert_eq!(queued.len(), 1, "exactly one expansion queued");
        let message = &queued[0];
        assert!(
            message.starts_with(SKILL_INVOCATION_PREFIX),
            "the expansion must ride under the marker: {message}"
        );
        assert!(
            message.contains("Write release notes for v2.4.0."),
            "$ARGUMENTS must be substituted: {message}"
        );
        assert!(
            !message.contains("$ARGUMENTS"),
            "no placeholder may survive: {message}"
        );
        // The ack is a pointer, not the body — the body rides the user
        // message, once.
        match out {
            ToolOutput::Ok { content } => {
                assert!(
                    !content.contains("Lead with user impact"),
                    "armed inline must not duplicate the body into the tool result: {content}"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        // The invocation is tracked for the compaction seam (#2685) and the
        // message is recognized as protected while the span lives.
        assert!(tools.active_invocations().protects_message(message));
    }

    /// An unwired surface must not lose the instructions: unarmed, the tool
    /// result itself carries the expansion.
    #[test]
    fn inline_invocation_falls_back_to_the_tool_result_when_unarmed() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(ws.path(), "audit", "", "Audit the module for $ARGUMENTS.");
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());

        let out = invoke_with_isolated_home(
            ws.path(),
            &tools,
            serde_json::json!({"skill": "audit", "arguments": "unsafe blocks"}),
        );
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with(SKILL_INVOCATION_PREFIX), "{content}");
                assert!(content.contains("Audit the module for unsafe blocks."));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(
            tools.injections().drain_steering().is_empty(),
            "nothing may sit in an unarmed queue"
        );
    }

    /// An inline skill's allowed-tools grant narrows this layer's surface
    /// for the rest of the turn, and teardown is structural (drop the tool
    /// stack, drop the layer).
    #[test]
    fn an_inline_grant_narrows_the_surface_until_the_stack_drops() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(
            ws.path(),
            "careful-read",
            "allowed-tools: read_file, grep\n",
            "Only read things.",
        );
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let active = tools.active_invocations();
        let out = invoke_with_isolated_home(
            ws.path(),
            &tools,
            serde_json::json!({"skill": "careful-read"}),
        );
        assert!(!out.is_error(), "{out:?}");

        assert!(tools.allows("read_file"));
        assert!(tools.allows("grep"));
        assert!(!tools.allows("bash"), "outside the grant is withheld");
        assert!(!tools.allows("write_file"));
        let refusal = tools.grant_refusal("bash").expect("bash must be refused");
        assert!(
            refusal.is_error() && format!("{refusal:?}").contains("careful-read"),
            "the refusal names the skill: {refusal:?}"
        );
        let mut schemas = FakeInner.schemas();
        tools.filter_schemas(&mut schemas);
        let names: Vec<String> = schemas.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["read_file".to_string(), "grep".to_string()]);
        assert!(active.is_active("careful-read"));

        // Structural teardown: the layers die with the per-turn stack.
        drop(tools);
        assert!(!active.is_active("careful-read"));
    }

    /// #2682 witness (c): a `context: fork` skill runs through the sub-agent
    /// runner and ONLY the child's result reaches the tool output — the body
    /// rides the child's charter, never the parent transcript. The
    /// frontmatter's effort override and resolved allowed-tools grant land on
    /// the spec.
    #[test]
    fn a_forked_skill_returns_only_the_result() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(
            ws.path(),
            "deep-audit",
            "context: fork\nallowed-tools: read_file, search\neffort: high\n",
            "SECRET-PROCEDURE: audit $ARGUMENTS thoroughly.",
        );
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let dispatcher = RecordingDispatcher::completed("audit finding: all clear");
        tools.set_dispatcher(dispatcher.clone());

        let out = invoke_with_isolated_home(
            ws.path(),
            &tools,
            serde_json::json!({"skill": "deep-audit", "arguments": "src/lib.rs"}),
        );
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("audit finding: all clear"), "{content}");
                assert!(
                    !content.contains("SECRET-PROCEDURE"),
                    "the body must never enter the parent transcript: {content}"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(
            tools.injections().drain_steering().is_empty(),
            "fork mode must not queue an inline expansion"
        );

        let spec = dispatcher.seen.lock().unwrap().clone().expect("dispatched");
        let charter = spec.system_prompt.expect("the body is the charter");
        assert!(charter.contains("audit src/lib.rs thoroughly"), "{charter}");
        assert_eq!(spec.effort, Some(stella_protocol::ReasoningEffort::High));
        assert!(!spec.write_access, "forked skills never get write access");
        // `read_file` granted by name; `search` is a catalog group — it
        // resolves to whichever of its tools the surface advertises (grep).
        assert_eq!(
            spec.allowed_tools,
            Some(vec!["read_file".to_string(), "grep".to_string()])
        );
    }

    /// A fork-mode skill with no runner attached is refused with a named
    /// error — never a hang, never a silent inline fallback.
    #[test]
    fn a_forked_skill_without_a_runner_is_refused_by_name() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(ws.path(), "fork-only", "context: fork\n", "Do the thing.");
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let out =
            invoke_with_isolated_home(ws.path(), &tools, serde_json::json!({"skill": "fork-only"}));
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("fork-only"), "{message}");
                assert!(message.contains("no sub-agent runner"), "{message}");
            }
            other => panic!("expected a named error, got {other:?}"),
        }
    }

    /// An unknown skill is a named error pointing at skill_search.
    #[test]
    fn an_unknown_skill_is_a_named_error() {
        let ws = tempfile::tempdir().expect("ws");
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let out =
            invoke_with_isolated_home(ws.path(), &tools, serde_json::json!({"skill": "nope"}));
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("skill_search"), "{message}")
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// Unknown directive VALUES degrade with a visible note, never a refusal
    /// (#2682 — graceful degradation end to end).
    #[test]
    fn unknown_directive_values_degrade_to_inline_with_a_note() {
        let ws = tempfile::tempdir().expect("ws");
        write_skill(
            ws.path(),
            "odd-skill",
            "context: detached\neffort: turbo\n",
            "Body text.",
        );
        let tools = InvokeSkillTools::new(ws.path().to_path_buf());
        let out =
            invoke_with_isolated_home(ws.path(), &tools, serde_json::json!({"skill": "odd-skill"}));
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with(SKILL_INVOCATION_PREFIX), "{content}");
                assert!(content.contains("unknown context"), "{content}");
                assert!(content.contains("unknown effort"), "{content}");
            }
            other => panic!("expected inline fallback, got {other:?}"),
        }
    }

    /// Object arguments render deterministically as sorted key=value pairs.
    #[test]
    fn object_arguments_render_sorted_and_deterministic() {
        let rendered = render_arguments(Some(&serde_json::json!({
            "target": "src/lib.rs", "depth": "full"
        })))
        .expect("objects are accepted");
        assert_eq!(rendered, "depth=full target=src/lib.rs");
        assert_eq!(
            render_arguments(Some(&serde_json::json!("plain text"))).unwrap(),
            "plain text"
        );
        assert_eq!(render_arguments(None).unwrap(), "");
        assert!(render_arguments(Some(&serde_json::json!(42))).is_err());
    }
}
