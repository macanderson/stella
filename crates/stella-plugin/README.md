# stella-plugin

The plugin manifest: parsing and validation of a plugin's declared say in the
turn loop — slice A of #3245 (plugins as turn-loop participants). One
constructor vouches for a manifest:

```rust
let manifest = stella_plugin::PluginManifest::from_toml_str(text)?;
```

A value that came back `Ok` has passed every rule the epic states for the new
blocks: the `[loop]` participation ladder (`none` < `observer` < `steering` <
`arbiter`, monotone — each grade includes the ones below), hook grants
(`Stop` only at `arbiter`, no hooks below `steering`), `max_holds` and
`[requirements]` as arbiter-only powers, the host-run `[oracle]` contract,
and `[subloop]`/`[roles]` as declared stages with routing *intents* — never a
credential or a URL. Unknown keys, unknown hook names, and unknown grades are
load errors (`deny_unknown_fields` everywhere, the #1400 rule this crate
inherits).

`[runtime]` (#3380, `doc:pipeline-as-plugins` §A5) is the process half: the
`argv` the host starts, the `timeout_secs` it enforces, and an environment
allowlist that is **default-deny** — the child inherits exactly the names
declared and nothing else. Two decisions are load-bearing. There is no
`language` field, because `["python3", …]` and `["node", …]` already
distinguish two plugins without stella learning what a language is (and three
plugins differing only in `argv` are what make the language-neutrality claim a
proof). And the environment is an exact list rather than a glob or a scrub, so
the set a user consents to at install is the set the child gets; a host-side
refusal of model credentials narrows it further, which this crate deliberately
cannot express because it has no credential vocabulary.

`[wrapper]` (#3381) is the turn-loop wrapper's stage order, declared instead
of hardcoded: an ordered `[[wrapper.stages]]` list under one variant id — the
id the store's `pipeline_variant` column records (#3388). Two properties make
it a gate rather than documentation:

- **The `if` field is a closed grammar**, not an expression language:
  `[no-]<boolean-signal>` or `<count-signal> <op> <number>`, over a published
  signal set, evaluated by a pure function. A condition naming a signal the
  host does not publish is a load error — a manifest that quietly does nothing
  is worse than one that refuses to load.
- **The stage graph is load-checked, on both axes.** A condition reading a
  signal that only a *later* stage publishes is rejected at load — and so is
  one reading a signal whose publisher is declared earlier but **conditional**,
  because that stage produces nothing on the turns it is skipped. A
  hand-written variant fails with a reason instead of wedging mid-run.

`Wrapper::resolve` is the reader that makes those rules load-bearing: the host
fills in `SignalValues` — one field per published signal, no `Default`, so an
unpublished signal is not representable, let alone silently `false` — and gets
back the ordered `StageProgram` that turn runs. The pair of graph rules above
is exactly what makes resolution **total**: a manifest that loads resolves for
every possible set of signal values, which `tests/wrapper_program.rs` asserts
as a proptest property rather than a promise.

Nothing here dispatches a stage: the four wrapper interception points are
#3380 and do not exist yet, so a stage name is a declared name that
load-checks and resolves. The crate takes no engine dependency by contract,
which is what lets the load-time contract be complete ahead of the socket it
describes.

`[oracle]` carries **two** shapes of evidence, and a manifest may declare
either or both. `flip = "required"` is the witness contract: the host credits
the requirement on an observed fail→pass transition. `flip = "not-applicable"`
plus `measurements` and `[[oracle.checks]]` is the other — the oracle reports
numbers and each check compares one of them against a budget, in the same
closed comparison grammar the `[wrapper]` conditions use. The second exists
because the first can express only one definition of done; a performance-budget
plugin (`tests/fixtures/perf-budget.toml`, the D-1 falsifier of
`doc:pipeline-as-plugins` §6.1) observes no flip at all. Two rules keep it
honest: a check reading a measurement the same `[oracle]` did not declare is a
load error, and dropping the flip *adds* an obligation rather than removing
one — under `not-applicable` every requirement must be decided by a check, so
the grade cannot be used to hold a turn open on vibes. `Oracle::unmet` is the
pure evaluator; running the process and decoding its numbers is the host's.

`[[capabilities]]` (`doc:pipeline-as-plugins` §A1) is the other half of a
consent document. `[loop]` says what a plugin may do *inside* a turn; this
says what it may reach *outside* one — a tool name, the grade it asks for in
`stella_protocol::RiskLevel` (the gate's own vocabulary, not a second one),
the reason a human reads, and any limit the plugin *claims* it will keep to.
It is gated on no participation grade, deliberately: a `none`-grade content
bundle shipping one custom tool that runs `git push` asks for more of the
world than an `observer` that only watches, and tying the list to the ladder
would let the widest grant hide behind the weakest grade.

`consent_text(&manifest)` renders both halves into the words an install
prompt shows, purely and deterministically — the "showable" half of A1's
requirement that the widest grant anyone will ask for (`gh`, the AWS CLI,
`brew`, a line in `~/.zshrc`, a daemon) be both expressible and visible
*before* install. Two properties make it a gate rather than a formatting
helper:

- **Nothing is elided.** Every declared tool, purpose and claimed limit
  reaches the output, with the widest grade as the headline.
- **The plugin owns the prose; Stella owns the structure.** Author-supplied
  text is flattened to one line and stripped of control characters, so a
  `description` carrying newlines cannot forge a line of the prompt and an
  ANSI escape cannot repaint the terminal the consent is given in.

A claimed limit is rendered as the plugin's claim and labelled as one: the
gate enforces the tool and the grade, and a prompt that showed a
self-declared scope as an enforced one would have the user consent to a
narrower grant than they are giving.

The one function a host must never bypass is
`LoopGrant::permits_hook(event)` — the authoritative filter behind the
epic's rule that **an undeclared hook is never invoked**, even if the
plugin's process registers for it. It gates on both the grade and the
declared list, so even a hand-built grant cannot leak a dispatch.

## Boundary — does this change belong here?

This crate owns one decision: *what a manifest declares, and whether that
declaration is coherent*. Pure functions over borrowed text; no I/O, no
environment, and exactly one workspace dependency: `stella-protocol`, for the
shared `HookEvent` vocabulary (#3310). That edge exists because it *removes* a
hand-kept mirror — the grant names the engine's dispatch points, and two
copies of one enum in two crates that may not depend on each other drift with
nothing red. `stella-protocol` is types-only, so it costs this crate no
behaviour; a second workspace dependency here needs the same argument made
again, not this one cited.

Everything that *acts* on a manifest is out:

- Reading the manifest off disk, *prompting* for install consent, lifecycle
  states, overlay and namespacing — the host (#1400's platform slices). The
  consent **text** is here (`consent_text`) because it is a pure function of
  what the manifest declared, and `stella-cli`, `stella-serve` and an
  embedded host must show a user the same words; asking the question, and
  turning a `yes` into gate rules, is theirs.
- Binding the grants to the engine's gates — the Stop gate, the hook runner,
  the sub-agent primitive — is the host's job (#3245 slices B/C). The engine
  itself never learns plugins exist: `stella-core` must never depend on this
  crate, and this crate must never depend on `stella-core`.
- Clamping `max_holds`, resolving `[roles]` tiers against the user's BYOK
  providers, running the oracle and tracking its flip — all host, all
  elsewhere.

`HookEvent` here is not a type of this crate's own: it is re-exported from
`stella_protocol::hook`, and so is `stella-core::hooks::HookEvent` (#3310).
The vocabulary lives underneath both because the dependency between them is
forbidden in both directions — which used to mean two hand-kept copies, and a
sixth engine event undeclarable in a manifest until someone remembered the
mirror. It is one edit now, and `hook_vocabulary_is_the_shared_one`
(`tests/manifest_grades.rs`) is the assertion that the sets cannot part.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it
before it crosses.

## Layout

- `src/manifest.rs` — the types (`PluginManifest`, `LoopGrant`,
  `Participation`, `HookEvent`, `Oracle`, `Subloop`, `Role`), parsing, and
  every cross-field validation rule, each documented on the `ManifestError`
  variant that enforces it.
- `src/wrapper.rs` — the `[wrapper]` block: `Wrapper`, `WrapperStage`, the
  closed `StageName` and `Signal` vocabularies, the `Condition` grammar and
  its parser, and the load-time stage-graph check.
- `src/program.rs` — the reader: `SignalValues` (the host's answer for every
  published signal, total by construction), `Condition::evaluate`,
  `Wrapper::resolve`, and the resolved `StageProgram`.
- `src/evidence.rs` — the `[oracle]` block's evidence half: `OracleCheck`,
  the `MeasurementRule` grammar and its parser, the load-time rules that keep
  a check readable and every requirement decidable, and `Oracle::unmet`.
- `src/observed.rs` — the line between what a plugin observes and what the
  host checks: `ObservedEvidence` (the `after_turn` payload, with no tamper
  field in any language) and `EvidenceSet::from_observed`, the host's merge.
  #3499 is why that is two types rather than one rule.
- `src/runtime.rs` — the `[runtime]` block: `Runtime`, its validation rules,
  and `Runtime::child_env`, the pure default-deny selection a host applies
  after clearing the child's environment.
- `src/consent.rs` — the install-consent surface: `Capability` (the
  `[[capabilities]]` entry and its validation), `highest_risk`, and
  `consent_text`, the pure renderer of the whole consent document.
- `src/error.rs` — `ManifestError`, typed per rule (invariant 5).
- `tests/manifest_grades.rs` + `tests/fixtures/*.toml` — slice A's
  acceptance: one fixture per grade, round-tripped through both TOML and
  `serde_json` (invariant 4), and the undeclared-hook filter proven against
  the fixtures.
- `tests/wrapper_stages.rs` + `tests/fixtures/wrapper-*.toml` — #3381's
  acceptance: the shipped stage order and a cheaper second variant, differing
  in nothing but their text, plus one rejection test per load rule.
- `tests/wrapper_program.rs` — #3408's acceptance for the reader: a manifest
  resolves into a stage order, and the proptest property that the two graph
  rules make that resolution total, deterministic and order-preserving for
  every set of signal values.
- `tests/install_consent.rs` + `tests/fixtures/self-driving.toml` — A1's
  acceptance: the widest grant anyone will ask for, written out, and the
  proof that it is both expressible here and fully shown before install.

## Consumers

`stella-pipeline` is the first, and only for the `[wrapper]` half: its
`src/variant.rs` embeds `variants/classic.toml`, fills in `SignalValues` from
one turn's facts, and resolves the built-in stage order through
`Wrapper::resolve` (#3408). It consults the answer; it is not yet *driven* by
it, because the four wrapper interception points are #3380.

The rest is still deliberately unconsumed: this is the first slice of #3245,
and the host for the other blocks (manifest loading, the install prompt that
shows `consent_text`, Stop-gate binding via the bounded verification loop, the
subloop runner) arrives with slices B–E of that epic and with
`doc:pipeline-as-plugins` §A4. The crate exists first because every one of
those slices needs the same validated answer to "what did this plugin
declare?" — and because A1's requirement is that the authority a plugin will
be granted is expressible and showable *before* the loader that grants it
exists, not after.
