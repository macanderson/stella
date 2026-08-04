# Design: `stella.toml` — one config file, and the five features it unlocks

**Status:** Phase 0 and Phase 1 implemented. Phases 2–6 unbuilt. ·
**Date:** 2026-07-30

**Built:** `toml_edit` comment-preserving writes
(`crates/stella-cli/src/settings/toml_io.rs`, §3.1); the `stella.toml` document and
its lowering into `Settings` (`settings/toml_config.rs`); three-scope
discovery with the project file at the **repo root** (§3.2, decided); the
dual read with TOML winning whole and the shadowed JSON announced (§6.1);
`[meta]` with `schema_version` and location-checked `scope`; the TOML
unrecognized-key vocabulary (`settings/unknown.rs`, §3.3); `api_key` refused
at project scope (§6.2); `[run].recap` replacing the bare root scalar (§6.3);
`[agents.<n>]` flattened with `[models].allowed` split out (§6.4); and
`stella migrate config [--dry-run]` (`settings/migrate.rs`), which runs
*before* provider resolution so a config too broken to start is still
migratable. Writes go through the same path, so `/theme`, the tool-switch
editor, and the engine editor all edit TOML in place once a `stella.toml`
exists.

**Not built:** the `[mcp.servers]` fold — the block parses and is announced as
inert rather than silently dropped, because consuming it crosses
`agent::load_mcp_plan`'s code-execution trust gate and belongs in its own
change (Phase 1b). Also unbuilt: everything in Phases 2–6 (pipeline stages,
per-agent tool scope, open agent set, `[models]` pin/track_latest, provider
fallback, integrations).

**Still read as before:** `.stella/mcp.toml`, `.stella/domains.toml`,
`.stella/tools/*.toml`, `~/.stella/credentials.toml`,
`~/.stella/integrations.json`, and project `.env` files. See
[`docs/design/filesystem-layout.md`](../../filesystem-layout.md) for the full
on-disk map.

**Companion files:** [`stella.today.toml`](./stella.today.toml) renders every
knob Stella reads *today* in the proposed shape (a pure transliteration).
[`stella.next.toml`](./stella.next.toml) renders the shape *after* the
features below are built. [`agent_system.toml`](./agent_system.toml) is the
original sketch this responds to.

**Decided:** the project-scope file lives at the **repo root**
(`./stella.toml`), not under `.stella/`. §3.2 has the reasoning; §6.2 has the
consequence that follows from it.

---

## 1. Goals, and one non-goal

**Goals.**

1. **One file, in a format that can hold its own documentation.** The
   semantics of `settings.json` currently live in Rust doc comments the user
   never sees — why `auto_mode` picks a different model family, why
   `headless_scope_bypass` defaults off, what `ann_enabled` trades away. JSON
   cannot carry any of it. That knowledge belongs next to the knob.
2. **A migration seam.** Today a typo and a future key are indistinguishable
   to serde, and the only defense is a warning printed by a hand-maintained
   key list (`settings/unknown.rs`). A `schema_version` turns forward
   compatibility from a convention into a mechanism.
3. **Make five capabilities configurable that currently are not**: pipeline
   stages (§4.5), per-agent tool scope and named agents (§4.1, §4.4), model
   policy over the catalog (§7.2), provider fallback (§4.2), and declarative
   integrations with multi-backend tools (§5).

**Non-goal: consolidating everything.** Four of the current files should stay
separate, and §7.1 says why for each. "One config file" means one file for
*configuration* — not one file for secrets, generated artifacts, and content.

---

## 2. What exists today, and the three invariants that must survive

The full key inventory is in `stella.today.toml`, verified key-by-key against
the serde structs. What matters here is that **the file format is not where
Stella's config safety lives** — three invariants do, and none of them are
visible in a JSON or TOML document. Any port that breaks one is a security
regression wearing a refactor's clothes.

### 2.1 Precedence and authority are orthogonal

`Settings::merge_captured_scopes` folds `[user, managed, project]` — so the
**project scope has the highest ordinary precedence**, not the org.

The org wins by two entirely separate mechanisms:

- **Trust rollback.** When the project is untrusted, specific fields are
  reset to the `[user, managed]`-only merge: `hooks`, `context_providers`,
  every provider's `base_url`/`api_key`/`api_key_env`, and
  `mcp.registry_url`. The project does not lose precedence; it loses *those
  fields*.
- **The ceiling.** `managed_tool_ceiling` is computed from the managed
  snapshot **alone**, so no later fold — including an explicitly trusted
  project — can raise it. `AuthorityPolicy` reads that captured value rather
  than re-deriving it, so the two cannot disagree.

A port that "simplifies" this into a single ordered chain (managed last,
wins everything) would look tidier and would be wrong: it would stop a
project from overriding a model choice, while doing nothing extra to stop it
from redirecting a credential.

### 2.2 Not every block merges the same way

Four distinct rules are in play, and the port must preserve each:

| Block | Rule |
|---|---|
| `providers`, `agents`, `context_providers` | per-entry, per-field overlay |
| `context`, `ui` | whole-block last-wins |
| `allowed_models` | replaces wholesale (one vocabulary; concatenating would stop a project narrowing the user's list) |
| `hooks` | **concatenates** — any scope may add a gate, none may remove another's |

### 2.3 Some values are privileged regardless of scope

A per-agent replacement `prompt` from an untrusted project is discarded and
restored from the trusted scopes unless managed `authority.project_prompts`
permits it. Same for `.stella/tools/*.toml` under `project_custom_tools`.
These are not tool switches — they are code and instruction injection
surfaces, gated separately.

---

## 3. Phase 1 — the format port

**Scope:** `settings.json` + `.stella/mcp.toml` → `stella.toml`. Semantics
frozen. No new keys except `[meta]`. The success criterion is that the
existing settings test suite passes against TOML fixtures with **no
assertions changed**.

This is ~60–70% mechanical. The merge logic is format-agnostic — it is
`overlay()` on typed structs, and `#[derive(Deserialize)]` handles TOML for
free. Three things are not mechanical.

### 3.1 Hazard: `save_to` will eat every comment

`AgentEngineConfig::save_to`, `ToolsSettings::save_to`, and
`UiSettings::save_to` all read-modify-write through a `serde_json::Value`
root, preserving unknown keys so a TUI save never drops `providers` or
`hooks`. That works because JSON has nothing to preserve *besides* keys.

TOML does. The workspace has `toml = "1.1"` and no `toml_edit`. Re-render a
parsed `toml::Value` and **every comment in the user's file is gone on the
first `/theme` change** — which deletes the entire reason for moving to TOML.

**Decision: add `toml_edit`, rewrite the three `save_to` implementations
against a `DocumentMut`, and gate it with a round-trip test that asserts
comments and key order survive a write.** This is the single most
underestimated task in the migration and should be built first, before any
key is ported.

### 3.2 Where the file lives

Recommended:

| Scope | Path |
|---|---|
| user | `~/.stella/stella.toml` |
| managed | `/Library/Application Support/stella/stella.toml` (macOS), `/etc/stella/stella.toml` (else), `STELLA_MANAGED_SETTINGS` override |
| project | `./stella.toml` at the workspace root |

The project file moving to the repo root is the one genuinely contentious
call. In favor: it is a committed, human-edited, reviewable file, and the
ecosystem convention for exactly that is the root (`Cargo.toml`,
`pyproject.toml`, `deno.json`). Against: it is one more root-level file, and
it makes the config *more* visible to a drive-by contributor.

That second point argues **for** the root, not against — the trust boundary
already assumes project config is attacker-controlled, and a config with real
authority implications is better reviewed in a PR diff than buried three
directories deep.

Consequence, and it is a real one: a root `stella.toml` is far more likely to
be committed than `.stella/settings.json` ever was. See §6.2.

### 3.3 The unknown-key walker needs a TOML twin

`settings/unknown.rs` walks a raw `serde_json::Value` against closed key
sets, descending into genuinely-open maps (`tools`, `providers`,
`context_providers`, hook matchers) and flagging the rest. It needs a
`toml::Value` equivalent.

Do **not** write a second walker. Both formats deserialize to a common shape;
convert `toml::Value` → `serde_json::Value` at the boundary and keep one
walker, one key list, one set of tests. The key lists are already shared with
the strict `STELLA_ENGINE_CONFIG_JSON` gate (`ENGINE_ROOT_FIELDS` and
friends), and that gate **fails closed** — a drifted second copy is a refused
benchmark run, not a missing warning.

---

## 4. Phase 2 — the agent plane

### 4.1 Open the agent set

Today `AgentEngineAgents` is a struct with four fields: `default`, `worker`,
`judge`, `triage`. A misspelled agent key is silently ignored.

Two things already exist that this closed set strands:

- The **`task` tool** (#922 → #976/#983) dispatches to one hard-coded
  read-only persona. Its input schema has `description` and `prompt` — there
  is no `agent_type`.
- **`.stella/agents/*.md`** holds named markdown personas, listed by
  `/agents`, otherwise advisory. They cannot set a model, an effort, or a
  tool scope.

Opening the set joins them: `[agents.<name>]` becomes a real engine posture,
and `task` gains an `agent_type` parameter that selects one.

**Migration:** `AgentEngineAgents` becomes `BTreeMap<String, AgentEngineAgent>`
with the four built-in names **reserved**. `EngineAgentKind` stays for the
built-ins (the router tiers on it, and `Role::Plan` rides the worker's
settings), gaining a `Custom(String)` variant.

**Cost of opening it:** the silent-ignore tolerance becomes a liability — a
misspelled `[agents.wrker]` currently does nothing loudly, and after this
change it defines a real but unreachable agent. `settings/unknown.rs` must
learn to flag an agent name that no `task` call and no pipeline stage
references.

### 4.2 Provider fallback (`provider_preference`)

An agent gets an ordered list instead of a single `provider`. First
*enabled* provider that can serve the model wins; a 429/5xx/timeout falls
through.

Three things must be true or this feature quietly corrupts the receipts:

1. **The resolved provider and model id are recorded per call.** A different
   provider serving "the same" model is a different tokenizer, a different
   cache, and different pricing. Cost attribution must follow the actual
   call, never the configured intent.
2. **Falling through invalidates the prompt cache.** The new provider has no
   cached prefix. This is a cost event and should be visible as one, not
   absorbed silently.
3. **`on_exhausted` is explicit.** `fail` (default) ends the turn;
   `degrade` falls back to the default agent's model. `degrade` must be
   opt-in — a judge silently dropping to a weaker model is worse than a
   clean failure, because the verdict still looks like a verdict.

### 4.3 `prompt_file`

An ergonomic win over inline prompt strings, and an authority hazard.

The inline `prompt` field is **privileged**: a project scope's is discarded
unless managed `authority.project_prompts = "on"`. A path indirection must
not become the way around that check.

**Rules:** `prompt_file` is gated by the **same** authority bit as `prompt`;
the path resolves inside the workspace root; symlinks are refused (the same
posture `settings/private.rs::reject_symlink` already takes on write); an
unreadable path is a named error, never a silent fall-through to the built-in
prompt — a judge quietly running the default prompt while the config says
otherwise is exactly the kind of invisible misconfiguration this system is
supposed to make impossible.

### 4.4 Per-agent tool scope

`[agents.<name>.tools]` uses the **exact same grammar** as the root `[tools]`
block — name > group > `"*"`. Not a similar grammar. The same
`ToolPolicy::from_switches` and the same precedence, because a second
tool-permission language is how permission bugs happen.

Composition, outermost first:

```
managed ceiling  ⊇  root [tools]  ⊇  [agents.<name>.tools]
```

An agent can only **narrow**. It can never grant a tool that the root block
or the ceiling denied. This needs a `ToolPolicy::intersect` and a property
test asserting the result is never more permissive than either input.

### 4.5 Pipeline stages

Today the staged pipeline (triage → plan → witness → execute → verify →
judge) is hard-coded in `stella-pipeline`, and `--no-pipeline` is the only
control — it turns the whole thing off and drops to the raw step-loop.

`[pipeline.stages.<name>]` gives each stage `enabled`, `on_disable`
(`skip` | `fail`), and where relevant `max_revisions` / `routes_to`.

**One `enabled` flag, not two.** The original sketch had both
`[agents.*].enabled` and `[pipeline.stages.*].enabled` answering "does this
run", and noted in its own comment that the pipeline one wins. Two flags for
one question desynchronize. Keep the pipeline's; `[agents.*]` has no
`enabled`.

`execute` takes `on_disable = "fail"` — there is no turn without it. That is
a config-validation error at load, not a runtime surprise.

**The stage vocabulary is not the six obvious ones.** `StageKind`
(`crates/stella-protocol/src/event.rs`) has eleven variants: `Triage`,
`ContextRecall`, `Plan`, `ScopeReview`, `Witness`, `Execute`, `Verify`,
`Judge`, `Reflect`, `ContextWrite`, `Complete`. `stella.next.toml` shows six
for readability; the real block must be built against the enum, not against
a hand-listed subset, or the two drift the first time a stage is added.

Three of the eleven need a decision rather than a default:

- **`ScopeReview` must not be configurable here.** `headless_scope_bypass`
  already answers "may an unattended run proceed past scope review". Adding
  `[pipeline.stages.scope_review].enabled` would be exactly the two-flags-one-
  question bug this section rejects — and on the highest-consequence flag in
  the file. Leave it out; `headless_scope_bypass` owns it.
- **`Complete` is terminal**, not a stage anyone turns off. Reject it at load
  the way `execute` is rejected.
- **`ContextRecall` / `ContextWrite` / `Reflect` already have owners** in the
  `[context]` block (`lifecycle.enabled`, `learning.mode`). Exposing them
  again under `[pipeline.stages]` creates a third overlapping authority.
  Either they stay out, or `[context]` delegates to them — not both.

This is the same failure mode as the sketch's double `enabled`, one level up:
the risk in a stage-configuration block is not that a stage is missing, it is
that a stage acquires a *second* switch.

---

## 5. Phase 3 — tools and integrations

### 5.1 `[integrations.<id>]`

Today `stella connect github|linear` runs an OAuth flow and writes tokens to
`~/.stella/integrations.json`. That is imperative *state*: there is nowhere
to declare "this workspace files tickets in THIS Jira project", and no way to
point a tool at a different backend without changing the tool.

This block holds connection **shape** only — endpoints, project keys, and the
*name* of an env var. Tokens stay in `integrations.json` and the environment.

Two requirements:

- **Fail fast at load** when an integration is `enabled` and the env var it
  names is unset. The alternative is discovering it mid-turn, when the tool
  fires, halfway through a run.
- **An integration is an egress destination.** A project-scope integration
  can send workspace content to a host the user never approved. It sits on
  the same trust boundary as `context_providers`: an untrusted project scope
  contributes nothing.

### 5.2 Multi-backend tools, and the heterogeneous-`[tools]` problem

Binding `create_follow_up_task` to `["jira-prod", "github-main",
"linear-ops"]` is the strongest idea in the original sketch. It decouples what
the agent *wants* from where it *lands*.

It makes `[tools]` heterogeneous: `bash = "off"` is a scalar,
`[tools.jira_search]` is a table. `ToolsSettings` is a flat
`BTreeMap<String, Toggle>` today.

**Rejected: `#[serde(untagged)]`.** Its failure message is "data did not
match any variant", with no indication of which field was wrong. For a config
whose entire posture is loud errors over silent fallback, that is the wrong
trade.

**Decision — Cargo's dependency shorthand:**

```
tool = "<toggle>"   IS EXACTLY   [tools.<tool>] enabled = "<toggle>"
```

A hand-written `Deserialize` that dispatches on the value *kind* (string vs
table) and reports the actual problem. The string form is sugar, not a second
dialect, so a two-line read-only agent stays two lines.

`ToolsSettings::save_to` must preserve table-form entries when the TUI
rewrites the switch map — the same read-modify-write contract as today, one
level deeper.

### 5.3 What is deliberately not built

**The registry stays a deny-list, not an enumeration.** The original sketch
enumerated 30 tools with `category` and `enabled`. Stella's tool set is only
knowable at runtime: built-ins, plus each connected MCP server's tools (names
known at connect time), plus every `.stella/tools/*.toml` manifest. An
enumerated config could not name an MCP tool at all, and would drift from the
built-ins every release. `category` already exists as the catalog's `group`.

---

## 6. Migration, schema decisions, compatibility

### 6.1 The migration

Read order during the transition:

1. `stella.toml` exists → use it. If `settings.json` also exists, **warn
   once** naming both paths. Never merge the two; a half-migrated config that
   silently composes is worse than either file alone.
2. Only `settings.json` → load it, and print a one-line pointer to
   `stella migrate config`.
3. Neither → defaults, exactly as today.

`stella migrate config` reads all three JSON scopes, writes the TOML
equivalents **with the explanatory comments** from `stella.today.toml`, and
leaves the JSON in place. It never deletes; the user removes the old file
once satisfied.

Key moves the migration performs, each warning on the old location for one
release:

| From | To |
|---|---|
| `agent_engine_config.*` | `[agents]` |
| `agent_engine_config.agents.<n>` | `[agents.<n>]` (flattened — see §6.4) |
| `agent_engine_config.allowed_models` | `[models].allowed` |
| `.stella/mcp.toml` `[servers.*]` | `[mcp.servers.*]` |
| `enable_recap` | `[run].recap` (see §6.3) |
| `trace_capture` | `[run].trace_capture` (#1042; same name, table home per §6.3) |

Deprecation window: **two minor releases** reading both, then JSON support is
removed. `schema_version` is what makes the removal safe — a file that
predates the field is version 1 by definition.

### 6.2 A root `stella.toml` will get committed

`api_key` (a literal secret) is an accepted field in `providers.<id>` today.
It is tolerable in `.stella/settings.json`. It is not tolerable in a file
sitting next to `README.md`.

**Decision:** `api_key` at **project scope** becomes an error naming
`credentials.toml` and `api_key_env` as the two right answers. At user and
managed scope it keeps working unchanged. Credentials never merge into
`stella.toml` at any scope.

The rule is enforced on **every path that reads or writes** a project
`stella.toml`, from one definition (`TomlConfig::validate_project_secrets`) —
not on load alone. Specifying it as load-only is what let the first
implementation ship a hole: `stella migrate config` serializes `providers`
verbatim, so with the check only in the loader it copied a key out of
`.stella/settings.json` into the committed file *and* produced a config the
loader then refused to read. A refusal that only fires after the secret is
already written is not a refusal. Any future writer of this file consults the
same method, before the write.

### 6.3 Retire bare root scalars

TOML attaches a bare key to whatever `[table]` precedes it. Drafting
`stella.today.toml`, `enable_recap` was written beside `[ui]` and silently
landed inside `[authority]`. It was caught only because
`ManagedAuthoritySettings` is `#[serde(deny_unknown_fields)]` — the one
strict struct in the whole schema. Anywhere else it would have parsed clean
and configured nothing.

This is a footgun aimed squarely at hand-edited config, and it is worth
designing out rather than documenting around.

**Decision: every scalar gets a table home.** `enable_recap` → `[run] recap`.
No bare keys at the document root, and a load-time lint that rejects them
with a message naming the table they belong in.

### 6.4 Flattening `agents.agents.<name>`

The JSON nests per-agent config one level below the flat model fields
(`agent_engine_config.agents.judge`). Rendered literally that is
`[agents.agents.judge]`, which reads badly.

Flattening to `[agents.judge]` is safe **only while the agent set is closed**
— four struct fields cannot collide with a root field. §4.1 opens that set.

**Decision:** flatten, and reserve the root field names as illegal agent
names, enforced at load with a named error. The list is exactly
`ENGINE_ROOT_FIELDS` minus `agents` (which disappears in the flattening):
`default_model`, `pipeline_judge_model`, `pipeline_worker_model`,
`pipeline_triage_model`, `allowed_models`, `auto_mode`, `effort_auto`,
`reasoning_auto`, `headless_scope_bypass`.

`allowed_models` stays reserved even though §6.1 moves it to
`[models].allowed` — it remains readable at the old location through the
deprecation window, and un-reserving a name is a breaking change you only
get to make once.

Reserve from the existing `ENGINE_ROOT_FIELDS` constant rather than a new
literal. That constant is already shared with the fail-closed
`STELLA_ENGINE_CONFIG_JSON` gate; a third hand-maintained copy would drift.

### 6.5 Generate the defaults, never hand-write them

`stella.today.toml` was drafted by hand and **seven of the ten
`[context.retrieval]` defaults were wrong** — plausible round numbers instead
of the real constants (`recency_weight` 0.3 vs 0.15, `ann_probes` 8 vs 12,
`lexical_limit` 20 vs 8, and four more). They were caught only by grepping
`crates/stella-context/src/retrieval.rs` afterwards.

That is not a drafting failure to be more careful about next time; it is
evidence about the artifact. A documented-defaults config file is a second
copy of every default in the system, and second copies drift silently — a
wrong default in a commented example is worse than no example, because it
reads as authoritative.

**Decision:** `stella migrate config` and any shipped example emit values
from `Default::default()` on the real structs, with the doc comments attached
to the *schema* rather than typed into the file. A test asserts the generated
example round-trips to `Settings::default()`. Nobody hand-maintains a defaults
list, ever.

---

## 7. Deferred and rejected

### 7.1 Files that stay separate

| File | Why |
|---|---|
| `~/.stella/credentials.toml` | 0600 from birth, redacting `Debug`, zeroized on drop, loose-permission advisory. Merging secrets into the most-committed file in the workspace inverts every one of those properties. |
| `.stella/tools/<name>.toml` | A tool ships **next to its script**. Inlining fifty manifests into one config breaks that locality and makes a tool un-copyable between repos. |
| `.stella/domains.toml` | Generated by `stella init`. Merging a generated artifact into a hand-edited file guarantees the generator eventually clobbers a human edit. |
| `.env` / `.env.local` | An ecosystem format with its own precedence rules (`.env.<mode>.local` → `.env.local` → `.env`, live shell always wins). Not ours to absorb. |

### 7.2 Rejected: `[models]` as a model table

The original sketch's `[models.<name>]` with `family`, `latest_alias`,
`pinned_version`, and `served_by.<provider>.model_id` duplicates the model
catalog — a SQLite store of cards, pricing versions, and aliases learned from
telemetry, refreshed from models.dev on explicit request and from
provider-native `/models`.

Two authorities for one question is a drift generator. Config holds *policy
over* the catalog (`allowed`, `pin`, `track_latest`); the catalog holds the
data.

### 7.3 Rejected: `track_latest` resolved per call

The sketch specifies resolution "at call time (or on a periodic refresh)".
For Stella that breaks three things simultaneously: prompt-cache prefixes (a
changed model id invalidates the cached prefix), cost comparability within a
session, and the benchmark contract that the system under test is frozen for
a run.

**Resolve once at session start; write the resolved id into the session's
receipts.** `stella models --resolved` shows what a session would pin now.

### 7.4 Deferred: provider `region` / `project_id` / `auth` / `api_version`

`Dialect::Vertex` and `Dialect::Bedrock` are built-in-only *because* they
need project/location/region resolution a settings entry cannot express.
Putting those fields in config means teaching
`stella_model::factory::build_provider` to construct those adapters from
config rather than from their env chains. That is its own project with its
own credential-chain implications; it does not belong in this one.

### 7.5 Rejected: array-of-tables for providers and integrations

`[[providers]]` cannot express the per-id, per-field overlay that lets a
managed `base_url` and a user `api_key_env` compose into one effective
provider. Reproducing it means matching on `id` and hand-rolling the merge —
more code, for a shape that reads worse. Keyed tables throughout.

---

## 8. Risks

| Risk | Mitigation |
|---|---|
| **Comment loss on save** silently deletes the migration's whole value | `toml_edit` + a round-trip test asserting comments and key order survive; build this first (§3.1) |
| **Trust model quietly weakened** by "simplifying" the scope chain | Port `merge_captured_scopes` unchanged; the existing trust tests must pass against TOML fixtures with no assertion edits |
| **Two config files both present**, silently composing | Never merge; warn once naming both paths (§6.1) |
| **Secrets committed** now that the file is at the repo root | `api_key` is an error at project scope on both the load and the migration path, from one shared check (§6.2) |
| **Per-agent tool scope grants rather than narrows** | `ToolPolicy::intersect` + a property test that the result is never more permissive than either input (§4.4) |
| **Fallback corrupts cost attribution** | Record resolved provider + model id per call, not the configured intent (§4.2) |
| **Opened agent set turns silent-ignore into silent-orphan** | Flag agent names no `task` call or pipeline stage references (§4.1) |
| **`stella.toml` at the root reads as an invitation** to configure things a repo should not | The trust boundary already assumes project config is hostile; §6.2 closes the one field that was tolerable only through obscurity |
| **Documented defaults drift from real ones** — already happened, 7 of 10 in one block | Generate examples from `Default::default()`; test the round-trip (§6.5) |

---

## 9. Sequencing

Each phase is independently shippable and independently valuable. Phase 1 is
a prerequisite for nothing except the pleasantness of the rest — the four
features could technically be built against JSON.

| Phase | Contents | Depends on |
|---|---|---|
| **0** | `toml_edit` round-trip harness + comment-preservation test | — |
| **1** | Format port, `[meta]`, `stella migrate config`, dual-read, unknown-key walker (§3, §6) | 0 |
| **2** | Pipeline stages (§4.5) | 1 (or JSON) |
| **3** | Per-agent tool scope (§4.4) + open agent set + `task` `agent_type` (§4.1) | 1 |
| **4** | `[models]` policy: `allowed` / `pin` / `track_latest` (§7.2–7.3) | 1 |
| **5** | Provider fallback (§4.2) | 1 |
| **6** | Integrations + multi-backend tools (§5) | 1 |

Phase 2 first among the features because it is the largest capability gain
for the least new surface: the stages already exist as code, and the config
only chooses which of them run.

Phase 6 last because it is the only one that introduces a new *category* of
config object, and it wants the migration and the trust plumbing settled
underneath it.
