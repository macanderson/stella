<!--
  Thanks for contributing to Stella! Keep it to one logical change per PR.
  Full expectations: CONTRIBUTING.md — the short version is the checklist below.
-->

## What & why

<!-- What does this PR do, and what problem does it solve? -->

<!--
  Replace the N below with the issue number, or delete the line if there is no
  issue. A number in the PR *title* does NOT close anything — GitHub only reads
  closing keywords from this description and from commit messages.
  Use `Refs #N` instead if this PR advances the issue without finishing it.
-->

Closes #N

## The witness

Stella's definition of done is a test that **fails on the old code and passes on the new**.

- [ ] This PR includes a witness test (fails on `main`, passes here), **or**
- [ ] No witness needed (pure refactor / docs / CI) — because:

<!-- If the witness is impractical (e.g. TUI rendering), say how you verified instead. -->

## The gate

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Docs updated where behavior/flags changed (README, `--help`, doc comments)
- [ ] CLA signed (the bot prompts on your first PR — nothing to do per commit)
- [ ] `Closes #N` appears **both** above and as a commit trailer — squash builds
      the commit body from commit messages, so the description alone won't carry it

## Nothing left behind

Anything you noticed and did not fix — a bug, a missing test, dead or unwired
code, the logical next step of this work — is a GitHub issue before this merges,
written as a handoff a fresh agent could execute (AGENTS.md § "Nothing left
behind"). The gate catches the residue (a `TODO` with no issue number); it
cannot catch what only you saw.

- [ ] Filed: #___, #___ — **or**
- [ ] There is nothing: everything I noticed is fixed in this PR

## Ground-rule check

<!-- Delete lines that don't apply. -->

- [ ] No I/O added to `stella-core`; no new deps without justification below
- [ ] No new outbound network calls (Stella never phones home)
- [ ] New cross-boundary types round-trip through serde (test included)

## Anything reviewers should know?

<!-- Risky areas, follow-ups deferred, alternative approaches you rejected. -->
