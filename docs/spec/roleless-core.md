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

**Landed so far — the vocabulary, both halves.** The consumer half is #3964:
`AgentEvent::Stage::name` is `stella_protocol::StageName`, so a stage outside
the host's twelve crosses the wire under its own word and every renderer draws
it. The producer half is #3963: `stella_plugin::StageName` is open the same
way, so `[[wrapper.stages]] name = "triage-lite"` loads, resolves and
dispatches — `stella_runtime`'s dispatcher asks `before_turn` for it by name,
and dropping the entry from the manifest drops the stage from the turn.

What that pair deliberately does **not** settle is the rest of this slice, and
the reason is §8.2/§8.3: it opens the vocabulary *within one wrapper's declared
order*. Composing the stages of **several** enabled plugins — the coarse band,
the resolved order written into settings, the tie-break on plugin id — is still
ahead, and so is the `points`/band metadata a stage would carry to attach
itself to something other than the position its manifest lists it in. A
contributed stage also publishes no signal: the signal vocabulary stayed closed
on purpose, because a plugin minting a fact for another plugin's condition to
read is a decision this slice has not made.

### Slice 4 — the config collapse (gap D)

**Splits in two, and the split is forced by a guard rather than chosen.**

**4a — the settings schema.** Delete `pipeline_<role>_model` (192 refs) and the
`agents.<persona>` map. `agent_engine_config` keeps `default_model`,
`allowed_models`, `auto_mode`, `effort_auto`, `reasoning_auto`,
`model_output_caps`, and `seat_models`. Old keys become a **named deprecation**:
recognized, ignored, and reported once with the seat assignment that replaces
them — never a silent drop, because these keys currently read like capabilities
and a silent removal keeps them reading that way.

Two constraints that are not obvious from the code:

- **The bench harness writes `pipeline_verifier_model` into Stella's settings**
  (`bench/harbor_adapter/stella_harbor/posture.py`,
  `bench/terminal_bench_analysis/tb21_posture_schema.py`), and a posture naming
  a verifier that does not resolve now refuses the run — added because the
  earlier behavior "left the run executing the control arm under a witness-arm
  digest" (#1147). Inert in the engine is **not** unused by the harness. The
  adapter and the posture schema migrate in the same PR, or the key stays.
- The TUI keeps its own mirror of the enum
  (`crates/stella-tui/src/envelope/engine_config.rs`). That envelope is a shared
  cell with slice 5; exactly one PR owns the shape change.

**4b — `EngineAgentKind` and `role_key()`.** Planned as blocked behind slices 2
and 6, on this forcing constraint: `scripts/check-role-names.sh` parsed
`role_key()`'s match arms out of `crates/stella-cli/src/config_wiring.rs` by awk
and **fails closed** when the function is gone, with a message that says in
terms "repoint this guard; do not delete it". `role_key()` takes an
`EngineAgentKind`, so the enum could not go while the guard read it, and the
guard could not go before slice 2.

**Landed with 4a instead, because the block dissolved on inspection.** The
constraint was never "the enum must survive" — it was "something must hold four
languages to one spelling". Deleting `role_key()` and repointing the guard at
`ENGINE_AGENT_NAMES` + `RETIRED_ENGINE_AGENT_NAMES` in
`crates/stella-cli/src/settings/unknown.rs` satisfies the guard's own
instruction literally (repoint, do not delete) and keeps the contract intact:
the union of the live name and the five retired ones is exactly the vocabulary
arenabench, the harbor adapter, the Observatory filter and the TUI's
`PipelineRole` still spell, and the guard still checks all four producers
against it. It reports `6 role(s) [default plan research triage verifier
worker]`, unchanged.

That is strictly better than leaving the enum standing: the words survive where
they are actually still true — as *retired spellings this workspace still
recognizes* — rather than as a live enum nothing routes on. When slice 6 (#3910)
retires the contract, `RETIRED_ENGINE_AGENT_NAMES` empties and the guard goes
with it, exactly as planned.

- **Witness:** a settings file carrying every retired key loads, warns by name,
  and changes no behavior.
- **Done when (4a):** no `pipeline_<role>_model` or `agents.<persona>` key
  exists, and the bench harness configures a seat rather than a pipeline pin.

**What shipped, against that bar.** The keys are gone from the settings schema,
and `EngineAgentKind` with them. The **bench harness was deliberately not
migrated**, and the retired keys are therefore *recognized* by the
trusted-launcher seam rather than refused — `RETIRED_ENGINE_ROOT` in
`settings::unknown` carries the argument. Migrating the harness re-hashes every
posture digest registered in `bench/READINESS.md` §8.4, which is the
published-numbers call #3870 reserves for a maintainer; it lands with slice 6,
where the Python stops writing the keys. Until then both doors name the keys out
loud, which is the half that ends the silence without spending a number.

Note that the #1147 protection the constraint above cites — "a posture naming a
verifier that does not resolve refuses the run" — no longer guards a *verifier*
pin: that one lived in `stella-pipeline` and went with the crate in #3865, and
there is no verifier role for a posture to pin any more (#3908). The
published-claim property it protected survived, and #3937 re-armed it at the pin
that still exists: under a trusted engine posture a `seat_models` entry that
cannot be built refuses the run rather than riding the session's model
(`agent::seats::EnginePosture`, consulted by `subagent::install_for_session`).
So `Config::engine_settings_trusted` is read again, and a harness posture whose
pinned seat does not resolve fails loudly instead of publishing a number under a
digest it does not describe.

### Slice 5 — the settings UI becomes a seat list (gap D)

**Parallelizable with 4a, with one shared cell.** The two live in different
crates — 4a in `crates/stella-cli/src/settings/`, this in `crates/stella-tui/` —
and meet only at `crates/stella-tui/src/envelope/engine_config.rs`, the mirror
of `EngineAgentKind` the driver populates. That envelope is the shared cell, and
the rule is the one this repository has learned three times over: **one PR owns
it, the other rebases.** This slice should own it, because it is the consumer
and the new shape is a consequence of what the pane needs to render.

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

## 8. Decisions — settled 2026-08-19

These blocked slice 3. All four are now decided; each subsection records the
decision and the reasoning, because the reasoning is what a later change would
be overriding.

### 8.1 Installed and active are two concerns

**Decided 2026-08-19 (Mac).** Installing a plugin puts it on disk and records
consent, and leaves it **inert**. A separate enable/disable writes to settings
and is what makes it participate in every turn.

- Install is often bulk or transitive; a command that reads like "download this"
  must not silently change every future turn.
- The enabled set is scoped config, so a project can enable a plugin for the team
  while an individual machine need not — the existing 3-scope merge, unchanged.
- Disable is a kill switch that is not uninstall. When a plugin misbehaves
  mid-incident, losing consent state to stop it is the wrong trade.
- The enabled set is the reviewable list slice 5's pane renders.

`--pipeline <id>` narrows in meaning: "run with exactly this, ignoring the
enabled set" — the per-run override that benchmark arms and experiments already
use it as.

### 8.2 A stage is a named ordering over the existing four points

**Decided 2026-08-19 (Mac).** Option (a). A manifest declares a stage's name, the
point it attaches to, and a coarse band; the turn loop composes the enabled set.
No fifth dispatch point.

- The four points are **demonstrably** sufficient for the stages we know we want:
  [`doc:turn-loop-wrappers`] §4 already maps every historical pipeline stage onto
  them (triage/recall/research/plan/scope → `before_turn`, witness →
  `after_turn`, verify → `judge`, revise → `again?`). That is evidence from a
  system that ran, not optimism about one that has not.
- A fifth point is a second wire contract to keep in step, with its own
  transports and admissibility rules, against a socket whose whole argument is
  that there is one contract.
- What is missing is not a point but **identity and order** — a name, so a stage
  can be shown, attributed and disabled individually; and a position, so N
  plugins compose. Both are manifest metadata over dispatch that already exists.

Option (b) stays available, taken when a **concrete** plugin needs something the
four points cannot express, with that plugin as the evidence.

### 8.3 Stage order is resolved into config, never observed from install order

**Decided 2026-08-19 (Mac).** A manifest declares a **coarse band**
(`early`/`normal`/`late`), not a fine-grained integer. On enable, the resolved
order is **written into settings** and read from there; ties inside a band break
on plugin id, lexicographically.

- **Install order is disqualified outright.** It is invisible, machine-local and
  not reproducible across clones, so the composed prompt would depend on the
  order someone happened to type commands — fatal for byte-stable prompts
  (`AGENTS.md` invariant 7) and for reproducing a benchmark run elsewhere.
- Manifest priority *alone* moves the fight rather than settling it: two authors
  both claim priority 0. A coarse band gives no ladder to climb.
- Writing the resolved order down, rather than recomputing it per run, makes it
  reviewable and diffable — and stops a plugin author reordering someone's turn
  by shipping a new manifest.

### 8.4 Seat names are always plugin-qualified

**Decided 2026-08-19 (Mac), reversing this document's first recommendation.**
A seat is `<plugin-id>/<role>`. There is no bare form and no precedence ladder:
resolution is one lookup, and a miss is the default model.

```toml
[seats]
"vera/test_author"    = "openrouter/openai/gpt-5.5"
"vera/verifier"       = "anthropic/claude-opus-5"
"stella-plan/planner" = "deepseek/deepseek-chat"
```

**The separator is `/`, because it is already this file's idiom.** The *values*
beside these keys are `provider/slug` model strings, and `parse_model_spec`
splits them on the first `/`. A reader who understands
`openrouter/openai/gpt-5.5` needs nothing new to understand `vera/verifier`.

Two alternatives were weighed and rejected, and the reasons are different in
kind:

- **A dot is dangerous.** In TOML, `vera.verifier = "…"` inside `[seats]` is
  *dotted-key syntax*: it parses as a nested table `vera` containing `verifier`,
  not as a flat key. It is correct only when quoted, and the unquoted form
  silently means something else in a config surface this project ships.
- **A double underscore is merely ugly**, and was argued for on a constraint
  that does not exist: seats have no environment-variable override and none is
  planned, so "survives an env var" was a hypothetical requirement invented to
  justify a separator.

Slash's one real cost is that TOML must quote the key. That is a **loud** cost —
an unquoted `vera/verifier = "x"` is a parse error the user sees immediately —
which is a different category from the dot's silent mis-parse. `crates/stella-cli/src/settings/toml_config.rs`
carries a round-trip test pinning it.

One constraint follows for slice 3: **a plugin id may not contain `/`**. Core
never splits a seat key — it compares whole strings — but the host constructs
one and a display surface reverses it, so the separator must be unambiguous at
those two points.

The bare form was recommended here first, for one-line convenience when several
plugins declare the same role name and want the same model. Three things killed
it:

1. **It breaches invariant 4 through a side door.** Assign `planner` for one
   plugin, install a second that also declares `planner` six weeks later, and the
   new plugin silently inherits a spending decision made about a different one.
   That is "installing a plugin multiplies the bill", arriving by the route a
   collision *warning* would have existed to paper over — and a mechanism whose
   safety depends on a warning firing is worse than one where the failure cannot
   occur.
2. **The namespace must be non-forgeable, and that requires the host to apply
   it.** A plugin declares its bare local name (`planner`) in its manifest and
   never writes the prefix; the host qualifies it. So a hostile plugin cannot
   declare `vera/verifier` and capture the assignment meant for Vera. Bare names
   have no equivalent protection — any plugin declaring `verifier` picks up
   whatever that word was assigned.
3. **The convenience is recoverable and the safety is not.** "Assign every
   planner at once" is a UI bulk action writing N explicit qualified keys.
   Un-sharing a bare key already written into a settings file is not.

Consequences: an assignment naming a plugin that is not enabled warns by name (a
detectable orphan, strictly better than a silent share); plugin authors no longer
need distinctive role names for safety, so `vera` naming a role `verifier` is
unambiguous rather than an invitation; and the resolution the slice-0 code
already ships — one map lookup, miss means default — is the final shape rather
than a stepping stone.

**The word `verifier` is banned from core, not from plugins.** A plugin may name
its roles anything; core compares the string and never interprets it.
`verifier` in a manifest is data. `Role::Verifier` in `stella-core` is the thing
being deleted.

## 9. How we will know it worked

Not "the words are gone" — that is checkable and insufficient. The test is §2's
sentence, run as an acceptance script: install a plugin declaring one stage and
one role, activate it, run a turn, and observe exactly two things — the turn's
composition changed, and the model count did not. Then assign a model to the
role and observe the inverse: the model count changed, and the composition did
not.
