---
id: roleless-core
title: "A core with one role: how the turn stops knowing what a triage is"
status: living
---

# A core with one role: how the turn stops knowing what a triage is

**Status:** living, written 2026-08-19. The target state and the plan; slice 0
has landed, the rest has not. Companion to [`doc:turn-loop-wrappers`], which
settled *that* wrappers are plugins and what the socket's four points are. This
document settles the half that one left open: **what vocabulary the core is
allowed to know**, and how a repository with 276 references to a six-name role
enum gets to zero without a flag day.

**Tracked as:** epic #3903, slices #3905 (1), #3906 (2), #3907 (3), #3908 (4),
#3909 (5), #3910 (6), #3911 (7).

---

## 1. The target, in one paragraph

Stella's core knows exactly one role, and its name is `default`. There is one
default model. Every other participant in a turn exists because an installed
plugin declared it, is named in a word the plugin chose, and runs on the
default model until a human says otherwise. Core compares those words and never
interprets them: there is no list of roles it accepts, no meaning attached to
`triage` or `planner` or `reviewer`, and no behavior that changes because a name
matched a literal. A plugin does not extend a taxonomy core published; it brings
its own, and core's ignorance of it is the feature.

## 2. The worked example this is measured against

> Install a triage plugin. Activate it. With no other configuration, the only
> thing that changes is that the turn now has a triage stage, and the default
> model performs it.

Every claim below is in service of making that sentence literally true, so it is
worth spelling out what it forbids as much as what it requires:

| Step | What must happen | What must NOT happen |
|---|---|---|
| `stella plugin install triage` | Consent names the stage it adds and the roles it declares | Core recognizing `triage` as a word it knows |
| Activate | The turn gains a stage, ordered by the plugin's manifest | Core carrying a slot named `triage` that was empty until now |
| First run, no config | The stage runs on the **default model** | A second model, a second bill, or a "sensible default" core chose |
| `seats.triage = "deepseek/deepseek-chat"` | The same stage, that model | Any change to the stage, its order, or its authority |
| Uninstall | The stage is gone; the turn is what it was | A residual key, a stale receipt role, or a settings warning |

The fourth row is the one that makes this a product rather than a refactor: the
user's only decision is *which model*, and they make it per role name, in one
place, for any plugin they ever install.

## 3. The invariants

Numbered because later work will cite them. Append; do not renumber.

1. **One core role.** Core's vocabulary is `default`, and `default_model` is the
   one model setting it ships. Any other model a session uses is one a human
   assigned to a name a plugin declared.
2. **Names are opaque.** A role name is compared, never parsed, normalized,
   validated against a set, or matched against a literal, anywhere in
   `crates/`. A name no one has ever used must route exactly as well as one
   shipped in an example.
3. **Zero built-in stages.** Core ships the step loop. Every stage over it —
   triage, planning, research, verification, anything — arrives as a plugin or
   does not exist. (This is [`doc:turn-loop-wrappers`] §4's "zero built-in
   wrappers", restated as a property of the vocabulary rather than of the code.)
4. **Free by default.** An installed, activated plugin with no seat assignments
   changes *what the turn does* and never *what it costs per call*: every role
   it declares resolves to the default model. Installing a five-participant
   plugin must not quietly multiply anyone's bill.
5. **Assignment is the user's act.** A plugin describes the process it needs; it
   may not request, prefer, or default a model. Whether a declared role runs on
   a model of its own is a human decision, recorded in the user's own settings.
6. **No default core substitutes.** For a role core does not understand, the
   only honest fallback is the model the session is already paying for. Core may
   never choose a different one on the user's behalf — a plausible-looking
   default here is core having an opinion about a role it was supposed to be
   ignorant of.
7. **Receipts describe spend, not process.** The closed enum that groups model
   calls for a cost report is core's and stays core's, but it describes *core's
   own* calls. A plugin's call is attributed to the plugin and the name it used,
   carried as data.
8. **The vocabulary is not an export.** No role word appears in a
   cross-language contract, a guard that pins spellings, a benchmark schema, or
   a dashboard's hardcoded filter list. Anything consuming role names reads them
   from the data.

## 4. Where we actually are

Counted on `main` at 2026-08-19, not estimated:

| Surface | Evidence |
|---|---|
| `EngineAgentKind` (`Default`/`Worker`/`Verifier`/`Triage`/`Research`/`Plan`) | **276 references** across `crates/` |
| `pipeline_<role>_model` settings keys | **192 references** |
| Rust files naming `verifier`, `triage` or `witness` | **505 files** |
| Non-Rust surfaces naming them (arenabench, bench, observatory assets, website docs) | **239 files** |
| `stella_protocol::Role` | 8 variants; `Triage`/`Plan`/`Research`/`Verifier` are routing vocabulary in the protocol crate |
| `stella_protocol::ModelCallRole` | 14 variants incl. `WitnessAuthor`, `WitnessRepair`, `Verdict`, `PlanRepair`; **on the wire**, exported into `docs/wire/` |
| `ChildTurns::default_seats()` | a **core-owned table of plugin role words** (`worker`/`triage`/`research`/`plan`), so a plugin needing a `reviewer` is refused |
| `Router::resolve_with` | matches on `Role::Triage` / `Role::Verifier` / … to pick a tier |
| `scripts/check-role-names.sh` | a **gate step** pinning four role spellings across Rust, Python and JS |

That last row is the one that reframes the job. The vocabulary is not an
internal detail that leaked; it is a **published contract with a guard enforcing
it**, with `role_key()` in `crates/stella-cli/src/config_wiring.rs` named as its
normative home. Removing the words means retiring the contract, and the guard
exists because a previous rename silently desynchronized a benchmark and
published a number for a pairing it never ran. Whatever replaces it has to close
that hole by construction — by making the names *data* that travels with the
run — rather than by deleting the guard and hoping.

### 4.1 What already landed

**Slice 0 (done, this branch).** The routing half, keyed on an opaque name:

- `SubAgentSpec::seat: Option<String>` — the requester's own word, which
  nothing in `stella-core` may branch on.
- `ChildTurns` forwards a plugin's declared role name onto the spec, beside the
  unchanged receipt role.
- `crates/stella-cli/src/agent/seats.rs` resolves an opaque name to a provider
  from the user's assignments; `SessionSubAgents` dispatches against it.
- `agent_engine_config.seat_models` and TOML `[seats]` carry the assignments —
  its own table, because `[agents]`'s flattening is only safe for a closed set
  and plugin-chosen names are an open one.
- A miss — no seat named, no model assigned, or an assignment that would not
  build — resolves to the session's model (invariants 4 and 6).

Two witnesses hold it: a named seat routes to its model while an unassigned seat
and an unnamed child ride the session's; and a name core has never heard of
routes exactly as well as one it has — which fails the day someone adds a list
of accepted roles.

**What slice 0 did not do:** it added the seat plane beside the old vocabulary
rather than in place of it. Nothing above has been deleted, and `default_seats()`
still gates which words a plugin may use. That is the rest of this document.

## 5. The four gaps

- **A. Routing vocabulary.** *Mostly closed by slice 0.* What remains is
  `ChildTurns::default_seats()` — core's list of acceptable plugin role words —
  and `Router`'s per-tier match arms.
- **B. Stage vocabulary.** The socket has four points (`before_turn`,
  `after_turn`, `judge`, `again?`) but no notion of a **named, ordered stage** a
  plugin adds. "Install triage and the turn gains a triage stage" needs stage
  identity and manifest-declared ordering, not just a hook that fires.
- **C. Receipt vocabulary.** `ModelCallRole` names jobs core no longer performs
  (`WitnessAuthor`, `Verdict`, `PlanRepair`). It is on the wire and
  schema-exported, so this is the one gap with a compatibility cost.
- **D. Config, UI and cross-language vocabulary.** `EngineAgentKind`, the
  `pipeline_<role>_model` keys, the TUI's six persona tabs, and
  `check-role-names.sh`'s four-language contract.

## 6. The plan

Seven slices. Each is independently shippable, independently revertible, and
carries its own witness. The ordering is a dependency order, not a priority
order — B is the most *valuable* and D the most *visible*, but A2 and C unblock
them.

### Slice 1 — `ChildTurns` stops owning the word list (gap A)

Delete `default_seats()`. A plugin's declared role resolves through the seat
plane and nothing else; core's only question is whether the plugin was *granted*
the ability to spend a child turn, which is a consent question and not a naming
one. The independence rule that `default_seats()` was carrying — a plugin may
not silently buy the worker's seat — moves to where it belongs: an explicit
grant checked against the manifest a human consented to, expressed without
reference to any role name.

- **Witness:** a manifest declaring a role named `reviewer` runs a child turn;
  today it is refused `Unavailable`.
- **Done when:** no literal role word appears in `crates/stella-runtime/`.

### Slice 2 — receipts carry the name (gap C)

`ModelCallRole` keeps only the calls **core itself** makes and loses the ones it
does not: `WitnessAuthor`, `WitnessRepair`, `Verdict`, `PlanRepair`, `Triage`,
`Research`, `Plan` describe a pipeline that no longer exists in this workspace.
A plugin's call is attributed as *the plugin, plus the seat name it used*,
carried as data alongside the enum.

This is a wire change: a new variant needs a `consumers.rs` row (an `E0004`
build error otherwise), the schema regenerates, and retired variants need an
alias path so an old recorded stream still reads. The prize is invariant 8 — the
role name travels **with the run** as data, so a benchmark reading it from the
trace can never desynchronize from a spelling in another language, which is the
exact failure `check-role-names.sh` exists to prevent.

- **Witness:** a recorded stream containing a retired variant still parses; a
  plugin child turn's receipt names its plugin and seat.
- **Done when:** `ModelCallRole` names no job core cannot perform.

### Slice 3 — stages are declared, named and ordered (gap B)

The socket gains stage identity: a manifest declares the stages it contributes,
their names, and where they sit relative to the turn. The turn loop composes the
installed set in manifest-declared order and dispatches each. This is the slice
that makes the §2 sentence true — "the turn now has a triage stage" — and it is
the largest.

Two decisions must be settled before writing code, and both are named in §8.

- **Witness:** installing a plugin declaring one stage changes the turn's
  composition, observably, with no configuration; uninstalling restores it
  exactly.
- **Done when:** a stage's existence, name and order come from a manifest and
  nothing else.

### Slice 4 — the config collapse (gap D)

Delete `pipeline_<role>_model` (192 refs), `agents.<persona>`, and
`EngineAgentKind` (276 refs). `agent_engine_config` keeps `default_model`,
`allowed_models`, `auto_mode`, `effort_auto`, `reasoning_auto`,
`model_output_caps`, and `seat_models`. Old keys become a **named deprecation**:
recognized, ignored, and reported once with the seat assignment that replaces
them — never a silent drop, because these keys currently read like capabilities
and a silent removal keeps them reading that way.

- **Witness:** a settings file carrying every retired key loads, warns by name,
  and changes no behavior.
- **Done when:** the only role name in `crates/stella-cli/src/settings/` is
  `default`.

### Slice 5 — the settings UI becomes a seat list (gap D)

The TUI's `AGENTS` pane loses its six compiled-in persona tabs and gains what
the `TOOLS` pane already has and it does not: **rows from the live session**.
Installed plugins' declared seats, each with the model assigned to it or
`default`. A session with no plugins shows one row.

`crates/stella-tui/src/views/tools.rs` is the pattern to copy verbatim — its
rows come from the driver precisely because MCP and custom tools "exist nowhere
but the assembled session stack", which is exactly true of plugin seats.

- **Witness:** a golden frame with a plugin installed shows its seat; with none
  installed shows only the default model.
- **Done when:** no role name is compiled into `stella-tui`.

### Slice 6 — retire the four-language contract (gap D, invariant 8)

Delete `check-role-names.sh` and its `GATE_STEPS` entry **only once slice 2 has
landed**, because slice 2 is what makes the deletion safe: with role names
travelling as data in the trace, arenabench and the bench analyzers read the
name that actually ran instead of copying a spelling. Repoint
`crates/stella-observatory/src/assets/index.html`'s hardcoded filter list and
the bench schemas at the same data.

- **Witness:** a bench analyzer fed a trace with an unfamiliar role name reports
  it rather than dropping the row.
- **Done when:** no role spelling is duplicated across languages, so there is
  nothing left for a guard to pin.

### Slice 7 — goal and monitor leave core (gap B, and the last one)

`stella goal`'s round loop and its cross-family verifier are the last stage core
performs, and the last multi-model path it ships. They become the reference
plugin. Blocked on [`doc:turn-loop-wrappers`] §9.2's unsettled encoding: the goal
verifier's **free-text feedback** — which is what steers the next round — has no
slot in `EvidenceSet`'s flip/tamper/measurement vocabulary.

- **Done when:** `stella goal` resolves to an installed plugin, and
  `reject_arbiter_wrapper_on_goal` is deleted because there is no longer a
  built-in arbiter to collide with.

## 7. What this deliberately does not change

- **`allowed_models` stays**, and becomes the ceiling on seat assignments too. A
  plugin may declare any role; the models a user can point at one remain the
  user's list.
- **Consent gets stronger, not weaker.** A plugin declaring more participants is
  declaring more of the world it wants, and install consent must render the
  stages and roles it adds. Opaque to core is not opaque to the human.
- **Receipts stay closed for core's own calls.** Invariant 7 narrows
  `ModelCallRole`; it does not open it to arbitrary strings. Cost reporting over
  a free-form enum is how a spend report stops being auditable.
- **The default stays one model.** Nothing here adds a second core model. The
  count of models a session uses is exactly one plus the number of seats a human
  assigned.

## 8. Decisions still open

These block slice 3 and should be settled before it starts; each is a genuine
fork, not a detail.

1. **Is "activated" distinct from "installed"?** §2 says "install … activate",
   which implies a plugin can be present but inert. Today `--pipeline <id>`
   names one wrapper per run. A standing activation set is a different model
   with different consent, ordering and failure semantics.
2. **What is a stage, formally?** Either (a) a named ordering over the existing
   four points, which is cheap and composes with what shipped, or (b) a new
   dispatch point with its own contract, which is more expressive and more to
   get wrong. (a) is the recommendation unless a concrete plugin needs (b).
3. **Do stages compose or conflict?** Two installed plugins both declaring a
   stage "before the worker" need a deterministic order. Manifest-declared
   priority, install order, and explicit user ordering are all defensible; a
   nondeterministic one is not, given byte-stable prompts (invariant 7 in
   `AGENTS.md`).
4. **Seat namespacing.** Slice 0 made a seat name a bare string, so two plugins
   both declaring `planner` share one assignment. That is either a feature
   (assign once, applies to any plugin's planner) or a collision (two plugins
   mean different things). Recommendation: keep bare names, because the user is
   assigning to a *concept they recognize* rather than to a plugin's internals —
   and revisit only when a real collision is reported.

## 9. How we will know it worked

Not "the words are gone" — that is checkable and insufficient. The test is §2's
sentence, run as an acceptance script: install a plugin declaring one stage and
one role, activate it, run a turn, and observe exactly two things — the turn's
composition changed, and the model count did not. Then assign a model to the
role and observe the inverse: the model count changed, and the composition did
not.
