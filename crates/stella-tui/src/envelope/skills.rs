//! The SKILLS tab's read models and the operations it routes to the driver.
//!
//! Split out of `envelope.rs` (#3493) under #629's 1500-line ratchet. The
//! rows are a driver snapshot, deliberately decoupled from
//! `stella_core::skills::Skill` so this crate stays independent of the skills
//! engine; the driver owns the disk, `npx`, and the model, and the deck emits
//! a [`SkillOp`] and folds the refreshed snapshot back.

/// Which scope a skill lives in / is installed to. The loader reads both
/// (`<workspace>/.stella/skills` and `~/.stella/skills`); the SKILLS
/// tab asks this on install/create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillScope {
    /// `<workspace>/.stella/skills` — travels with the repo.
    Project,
    /// `~/.stella/skills` — the user-global directory.
    User,
}

impl SkillScope {
    pub fn label(self) -> &'static str {
        match self {
            SkillScope::Project => "project",
            SkillScope::User => "user",
        }
    }
}

/// One installed-skill row in the SKILLS tab — a driver snapshot, deliberately
/// decoupled from `stella_core::skills::Skill` so the TUI crate stays
/// independent of the skills engine (the driver maps one to the other).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRow {
    pub scope: SkillScope,
    pub name: String,
    pub description: String,
    /// The pinned version's markdown body — carried so the tab can open the
    /// edit overlay with no extra round-trip.
    pub body: String,
    /// Provenance label — `"workspace"`, `"user"`, `"installed"`, `"auto"`,
    /// `"plugin"`.
    pub origin: String,
    /// How strong the evidence behind an `"auto"` skill actually is —
    /// `stella_protocol::provenance::ProvenanceGrade::as_str()`, e.g.
    /// `"model_critique"` or `"environment_observation"` — looked up from the
    /// proposal ledger by candidate id (#4871). `None` for a hand-authored,
    /// installed, or contributed skill, and for an auto-created one whose
    /// promoting proposal predates the ledger or has aged out of it: absent,
    /// not a weak grade the tab invented. Kept a plain label rather than the
    /// typed enum so this crate stays independent of `stella-protocol`, the
    /// same reason [`Self::origin`] is a `String`.
    pub evidence_grade: Option<String>,
    /// The plugin package that ships this skill, by its manifest `name`, when
    /// it is a contributed one; `None` for a skill the user wrote, installed,
    /// or had written for them.
    ///
    /// A field beside [`Self::origin`] rather than a third [`SkillScope`]:
    /// scope answers which tier a skill is *managed under*, and a contributed
    /// skill is genuinely managed under its package's tier — it is enabled and
    /// disabled by that tier's state file like any other row there. What has
    /// no answer in the existing columns is *whose* it is, which is this.
    pub contributed_by: Option<String>,
    /// Session/persistent enabled state (a disabled skill is excluded from
    /// recall injection; the file stays on disk).
    pub enabled: bool,
    /// The pinned/current version the recall path uses.
    pub version: u32,
    /// The highest version on disk (`version < latest` ⇒ pinned to an older one).
    pub latest: u32,
    /// Whether the deck may uninstall (delete) it.
    pub removable: bool,
}

/// One registry search hit, parsed from `npx skills find` into structured
/// fields — never the raw ANSI-laden line. `id` (`owner/repo@skill`) is the
/// display name AND the token passed verbatim to `npx skills add` / `use`;
/// `installs` is the human popularity string (`"15.8K installs"`, empty if the
/// registry printed none) with `installs_rank` its numeric form for ranking/
/// the popularity bar; `url` is the `skills.sh` page (shown in the preview).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillSearchHit {
    pub id: String,
    pub installs: String,
    pub installs_rank: u64,
    pub url: String,
}

/// The full installed-skills read-model rendered by the SKILLS tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillsView {
    pub rows: Vec<SkillRow>,
    /// A one-line status/outcome hint (last op result), or `None`.
    pub status: Option<String>,
    /// True while a driver op (npx search/install, LLM create) is in flight,
    /// so the tab can show a working state.
    pub busy: bool,
    /// The name of the skill a just-completed [`SkillOp::Create`] wrote,
    /// when this snapshot is that op's completion. The create dialog
    /// transitions into the ctrl+o preview of exactly this row; `None` on a
    /// failed create (the dialog shows `status` as the error) and on every
    /// other snapshot.
    pub created: Option<String>,
}

/// A SKILLS-tab operation routed to the driver, which owns the disk + npx +
/// model. The deck emits one and folds back the refreshed
/// [`Inbound::Skills`](super::Inbound::Skills) /
/// [`Inbound::SkillSearch`](super::Inbound::SkillSearch) snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillOp {
    /// Re-enumerate installed skills → [`Inbound::Skills`](super::Inbound::Skills).
    List,
    /// Persistently enable/disable a skill → re-send the list.
    SetEnabled {
        scope: SkillScope,
        name: String,
        enabled: bool,
    },
    /// Delete a skill entirely from disk (deck double-confirms first).
    Uninstall { scope: SkillScope, name: String },
    /// `npx skills find <query>` → [`Inbound::SkillSearch`](super::Inbound::SkillSearch).
    Search { query: String },
    /// Fetch a search hit's `SKILL.md` for the ctrl+o preview overlay
    /// (`npx skills use <id>`, extracting the wrapped body) →
    /// [`Inbound::SkillPreview`](super::Inbound::SkillPreview). No disk write —
    /// preview only.
    Preview { id: String },
    /// Install a registry skill into `scope` → refresh the list.
    Install { scope: SkillScope, id: String },
    /// LLM-assisted creation from a short description → refresh the list.
    Create {
        scope: SkillScope,
        description: String,
    },
    /// Save an edited body as a NEW version (increments + pins it).
    Edit {
        scope: SkillScope,
        name: String,
        body: String,
    },
    /// Pin `version` as the active one WITHOUT editing (no version bump).
    Pin {
        scope: SkillScope,
        name: String,
        version: u32,
    },
}
