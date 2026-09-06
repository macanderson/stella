# stella-learn

What the agent learns, and what steers it.

These module trees do the work.

- `skills` — the skill catalog. Read a `SKILL.md` file. Pick the skills a
  prompt needs. Render the block the prompt carries. Mine new ones out of
  past lessons.
- `rules` — the rule engine. Read a rule file. Merge rules by rank. Render
  the soft tier. Check the hard tier at the tool edge.
- `mining` — the text miner both of those share. It groups free-text
  lessons, scores their words, and names a group. There were two copies of
  it once. One copy is the point.
- `comparison` and `self_tuning` — is arm B better than arm A? One report
  shape and one stats test, for every A/B this project runs.
- `holdout` — the schedule that makes the arm they compare against. On some
  turns it names one item to leave out, so that item gets a control arm. The
  item is an opaque id, so one schedule serves skills, memories and rules.
- `ledger` — one row of the trial ledger, and the key it is filed under: a
  kind and an id. A row can name a memory, a rule or a skill, so the three
  surfaces share one ledger rather than keeping one each.
- `redact` — strip secrets out of text before it leaves the machine.

## Boundary

No I/O. Every entry point is a plain function. The caller reads the file and
passes the text in. Nothing here opens a file, starts a process, or reaches
the network. `RuleSource` and `SkillSource` are the ports for that, and
`stella-cli` implements them.

This code lived in `stella-core` once. It left because the engine does not
use it, which is the twelfth rule in AGENTS.md. The step path reached one
thing here: how a skill is invoked. That part stayed, as
`stella_core::skill_invocation`. The driver writes an invocation message on
restore, and receipts read its marker, so those words are engine words.
`stella-core` now names no skill, no rule, and no report type.

`stella-protocol` is the only workspace crate this one uses. Three small
parts live down there so the engine can read them too: the token estimate
the skill budget spends, the `---` header parser, and the glob a rule guard
matches on.

`scoreboard` arrived the same way and for the same reason: the engine never
read it. It is the yardstick `comparison` and `self_tuning` read — four
numbers per unit of delivered work, none of them judged by a model.
`stella-cli`'s `scoreboard_cmd` renders them.

Why a crate and not a module: AGENTS.md § "When a new crate is justified",
clause (b). `stella-records` needs the rule parser and the redactor, and
`stella-core` must not be what hands them over.

## Where a change goes

| You want to… | File |
|---|---|
| Change how a `SKILL.md` is read | `src/skills.rs` |
| Change which skills a prompt picks | `src/skills.rs`'s `select_skills` |
| Change the rendered skill block | `src/skills.rs`'s `render_skills_section` |
| Change how a skill is judged and kept or dropped | `src/skills/appraisal.rs` |
| Change where skills are looked for | `src/skills/paths.rs` |
| Change how a rule file is read or merged | `src/rules.rs` |
| Change when a guard blocks a tool call | `src/rules.rs`'s `evaluate_guards` |
| Change how lessons group into a candidate | `src/mining.rs` |
| Change what counts as a secret | `src/redact.rs` |
| Change the A/B report shape | `src/comparison/report.rs` |
| Change the stats test | `src/self_tuning.rs` |
| Change how often a holdout fires, or which item it picks | `src/holdout.rs` |
| Add a kind of artifact the trial ledger can hold | `src/ledger.rs` |
| Change what one unit of delivered work is scored on | `src/scoreboard.rs` |

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Testing

```bash
cargo test -p stella-learn
```

Every test is an inline `#[cfg(test)]` module, or a `tests.rs` beside the
file it covers. `proptest!` blocks live in `src/skills.rs`, `src/mining.rs`,
`src/self_tuning.rs` and `src/comparison/props.rs`. Past failing seeds are
kept in `proptest-regressions/`.

No feature flag, no env var, no fixture server, no network. A test here that
needs a file or a socket means the code under test belongs in `stella-cli`.
