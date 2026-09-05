# AGENTS.md

Guidance for AI agents (and humans) working in this repository. This is a
condensed orientation focused on the non-obvious conventions and invariants
that aren't immediately apparent from reading a single file. The authoritative
sources for the details behind each section are `README.md` and `CONTRIBUTING.md`.

Stella is a fast, BYOK ("bring your own key"), model-agnostic terminal coding
agent, written in Rust. Proving a task **done** with a **witness test** — one
that fails on the old code and passes on the new — is what "verified done,
not claimed done" means here, and that guarantee is **a property of the path
that produced the evidence, not of the binary**. The built-in staged
pipeline that used to run the check itself and watch the fail→pass flip has
been deleted from this workspace (#3865; `docs/spec/pipeline-as-plugins.md`
§7 names it "the last slice" of the extraction plan, and it has now landed) —
`stella run --pipeline classic` is refused outright, naming `stella plugin
install` as the remedy. Host-run verification no longer exists in this
workspace at all: the only verification path left is an **installed
verification plugin** — `stella run --pipeline <plugin-id>` hands the turn to
that plugin, whose evidence is self-reported. Stella evaluates that evidence
against the plugin's declared rule and never re-runs or re-checks it itself
(#3511; `doc:pipeline-as-plugins` is the extraction plan, and Oxagen's Vera is
the reference verification plugin, private and not shipped in this
repository).
Neither path runs by default — a plain `stella run` is the raw step-loop
with no verification stage over it. It is the open-source reference
implementation of
Oxagen's *Engineering Deterministic AI Coding Agents* field manual.

---

## Essential commands

The repo is a Cargo workspace. Rust is **pinned to a concrete version**
(currently 1.97.0) via `rust-toolchain.toml` (rustup fetches it automatically).
Floating on `channel = "stable"` was tried and reverted — each new stable
release ships a slightly different rustfmt, which silently reformats
previously-clean files and turns the CI fmt gate red with zero code changes.
When bumping the pin for a new Rust release, do it as one dedicated PR that
updates the version in `rust-toolchain.toml` and runs `cargo fmt --all` in the
same commit (or the next one) so drift never accumulates. A **`Makefile`**
wraps the common commands with the correct flags — run `make help` for the
full list.

```bash
make build               # cargo build --workspace
make test                # cargo test --workspace
make format              # cargo fmt
make lint                # cargo clippy --workspace --all-targets -- -D warnings
make smoke               # compile check — runs `stella models` (no API key needed)
make help                # list every target
```

**Iterate on a single crate** (much faster than the whole workspace):

```bash
make test-core           # or: cargo test -p stella-core
make test-model          # or: cargo test -p stella-model
make test-tools          # or: cargo test -p stella-tools
```

**Watch mode** (requires `cargo install cargo-watch`):

```bash
make watch               # re-run workspace tests on every save
make watch-core          # re-test stella-core only (fastest loop)
make watch-lint          # re-run clippy on every save
```

**Rustdoc, scoped to the crate you're editing.** `make gate`'s `doc-warnings`
step runs `cargo doc -D warnings --document-private-items` over the whole
workspace, which is what catches a `rustdoc::private_intra_doc_links` error —
a doc comment linking a crate-private item. Neither `make guards-fast` nor a
scoped `cargo clippy` builds docs, so neither one catches it. `CARGO_SCOPE`
narrows the same check to the crate at hand, the same way it narrows `lint`
and `test` above:

```bash
make doc-warnings CARGO_SCOPE="-p stella-core"
# equivalent to:
RUSTDOCFLAGS="-D warnings" cargo doc -p stella-core --no-deps --document-private-items
```

It compiles, so it stays a `make gate` step rather than moving into
`make guards-fast` — CARGO_SCOPE is what makes it seconds instead of the
full workspace.

### The gate — what every push is held to

A red gate is an automatic "not yet". CI is where it runs: on the
maintainer's laptop an agent session does not run `make gate`, a workspace
build, or the workspace test suite — it pushes and reads the run
(CLAUDE.md, "CI builds and tests; this laptop does not"). The list below is
the contract CI enforces and the command a contributor with their own
machine runs before pushing:

```bash
make gate                # = no-scratch + no-secrets + design-refs
                         #   + action-pins + cargo-install-pins
                         #   + license-allowlist-parity + repro-wiring
                         #   + shellcheck + invariants + doc-links
                         #   + adr-numbering
                         #   + command-docs + brand-case + file-size
                         #   + website-inputs (a Rust test's website/ inputs
                         #     are declared, and ci.yml's filter is built
                         #     from that declaration)
                         #   + god-files
                         #   + gate-parity
                         #   + schema-tier-parity (a -schema step runs at the
                         #     same rung as its base step; #5139)
                         #   + guard-trigger-coverage (prose, hue-separation
                         #     and transcript-surfaces each run with no
                         #     paths: filter in at least one workflow)
                         #   + priority-scheme (the issue priority scheme is
                         #     stated once, in SCR-005, and the triage guard's
                         #     regex covers exactly the levels it names)
                         #   + left-behind + role-names
                         #   + stat-portability + module-reachability
                         #   + core-reachability (a stella-core module is
                         #     reachable from the engine's step path; down-only)
                         #   + typed-errors
                         #   + tool-error-class (#3167 unclassified-ToolOutput::error ratchet)
                         #   + dead-code-allows
                         #   + measured-constants (a MEASURED: constant is pinned by a test)
                         #   + diagnostic-codes
                         #   + consumer-sites (Behavioral 'site' strings point at live code)
                         #   + bench-suites
                         #   + wire-paths (the hook derives wire-schema.yml's filter)
                         #   + tokens (hue clamp + no retired hex)
                         #   + hue-separation (30° OKLCH, web tokens)
                         #   + contrast (WCAG, down-only ratchet)
                         #   + light-clamp (a shipped light SURFACE against
                         #     the clamp its family declares — the other
                         #     three colour rulers all read the token table,
                         #     and so did both enforcers of `warm-paper`,
                         #     which is why that clamp governed nothing that
                         #     ships; down-only ratchet)
                         #   + transcript-surfaces
                         #   + prose (no content-free constructions added)
                         #   + line-citations (prose cites code by symbol
                         #     name; a pinned line number drifts silently)
                         #   + deck-fit-all-test (the deck enumeration)
                         #   + deck-paths (the decks' code-map citations)
                         #   + css-vars (every var() in a token sheet resolves)
                         #   + reserved-paths (no Windows device name in a path)
                         #   + rendering-facts (no v2 rendering draws a
                         #     fact SPEC.md retired)
                         #   + wire-schema
                         #   + lockfile-sync (cargo metadata --locked)
                         #   + format-check (fmt --check)
                         #   + doc-warnings (rustdoc -D warnings)
                         #   + doc-warnings-schema (the same, for the
                         #     `schema`-gated wire-contract modules the
                         #     default-feature run never compiles)
                         #   + lint (clippy -D warnings)
                         #   + lint-schema (the same, for those modules —
                         #     an `#[expect]` there is policed by nothing
                         #     else, because rustc does not evaluate a tool
                         #     lint's expectation with the tool absent)
                         #   + test (test --workspace)
                         #   + tool-docs (docs/tools/ vs the declarations)
                         #   + self-driving-test (the shell harness)
```

That list is not maintained by hand: it is `GATE_STEPS` in the `Makefile`, and
`gate-parity` (`scripts/check-gate-parity.sh`) fails if this block or
CONTRIBUTING.md's stops matching it. The block had already drifted twice before
that guard existed, both times by under-reporting a newly added guard, which is
the direction that misleads — a reader runs the short list, sees green, and
believes the gate is green (#1437).

Deliberately no total: the guard checks each step by name, so two PRs adding
different guards merge cleanly, while a spelled-out count is one shared cell
both must write — and two of them collided on it twice in a day, each time
leaving `main` red for everyone (#1883).

CI enforces the same steps split across four workflows:
`/.github/workflows/ci.yml`'s required job runs everything except `invariants`,
`doc-links`, `prose`, `hue-separation` and `transcript-surfaces`, and adds a
`Cargo.lock` sync check, `stella context
validate`, a release smoke build (thin LTO), and the deleted-test guard
(`scripts/check-deleted-tests.sh`);
`docs-guards.yml` runs those two plus a second run of `command-docs`,
`website-inputs`, `brand-case`, `gate-parity`, `god-files` and `design-refs`,
because all of them trigger on the `docs/**`, `website/**` and `*.md` paths
`ci.yml` skips — `website-inputs` was the last to join, after a website-only PR
was found able to move a file a Rust test reads and land green, leaving `main`
red on a test nobody had run (#4632, and #3888 is the same shape one boundary
over: a docs-only PR moving a document into the `docs/design` scratchpad and
invalidating every Rust comment citing it); and
`wire-schema.yml` runs `wire-schema` on `docs/wire/**` and the protocol crates,
because a PR that hand-edits a generated schema and nothing else starts neither
of the other two (#1439) — and `doc-warnings-schema` beside it, because
`ci.yml`'s `cargo doc --workspace` runs with default features and every module
that describes the wire format sits behind an off-by-default `schema` one, so
rustdoc compiled none of them anywhere (#4584); this workflow already builds
those three crates with the feature on; and `guard-self-tests.yml` runs the three steps
ci.yml's job cannot — it is skipped for a prose-only diff, which is the diff
`prose` exists to judge — alongside the hermetic suites that prove a guard can
still fail (#3820, #4427). Which workflow runs a step is a judgement; *that*
one does is checked, by `gate-parity` against every `run:` in
`.github/workflows/`. That check stops at "does some workflow run it" — it
says nothing about which files reach it, so a `paths:` filter narrowing
`guard-self-tests.yml`'s `pull_request:` trigger to `docs/**` would still read
as green, and the three steps would run nowhere for a scripts-only pull
request. Nothing held that absence in place except a comment.
`guard-trigger-coverage` reads the trigger back: each of the three must run
with no `paths:`/`paths-ignore:` filter in at least one workflow, so a later
edit narrowing this one cannot reopen the gap silently. That workflow's
`gate-steps` job — named `gate steps no other workflow runs` — is now one of
`main`'s required status checks too,
added after a red run of it merged into `main` and reddened every open PR.

A fifth workflow, `deck-fit.yml`, owns the decks under
`website/public/presentations/`. Its measurement — every slide against the
fixed 1600x900 canvas the decks are authored in (`scripts/deck-fit.mjs`) —
needs a browser, which is why that half is not in `make gate`, and it has its
own file rather than a job in `docs-guards.yml` because that workflow triggers
on `**/*.rs` and would launch a browser on nearly every PR — the same
disjoint-paths reasoning that gave `wire-schema.yml` its own file. It is
deliberately not a required check yet (#2425).

The browser is what is expensive, not the deck directory, so the two checks
over these files that need no browser are gate steps and run here as well:
`deck-fit-all-test` covers the enumeration the workflow drives
(`scripts/deck-fit-all.sh` — inline in the `run:` block until #3404, which is
why the enumeration bug #3376 had no regression test), and `deck-paths` holds
the decks' code-map citations to files that still exist. `deck-paths` runs in
this workflow rather than only in the gate because `ci.yml`'s required job
skips its Rust half for a diff confined to `website/**` (#1892), and a diff
confined to `website/**` is exactly a deck edit (#3573).

A sixth, `main-canary.yml`, is the only one that runs **after** the merge, and
it exists because some guards cannot be settled before one. A guard enforced
against a *shared cell* — one file every PR of a shape must write, like
`Cargo.lock` or `scripts/file-size-baseline.txt` — can be satisfied correctly by
two branches that still compose into a broken tree once both land. No pre-merge
run can catch that: neither author's tree is wrong. So the canary re-asks the
composition questions on `main` itself (push, plus a daily backstop for the
breakage no commit caused, such as a yanked dependency), and reports by opening
one labelled issue — closing it again when `main` recovers, because a monitor
that only ever files gets muted and is then worse than none (#1464 is what
silent failure costs here). `make main-canary` runs the same check locally
without filing anything; `scripts/main-canary.sh`'s header carries the full
argument, including why it deliberately does not open a fix PR (#3332).

It answers "is `main` **known broken**". A second step in the same workflow —
`scripts/check-main-verified.sh` (`make main-verified`) — answers the question
nothing else here asks: is there a commit on `main` that nothing **verified**?
Those are different states, and every other mechanism reads the second as
green. The canary files only when its own job runs and fails, so a
`startup_failure`, a cancellation, or a run that was never created produces no
issue; `main-red-hold.yml` passes when no issue is open; and `gh run list`
shows no row at all for a run that never existed. On 2026-08-26 an Actions
outage landed four commits with no completed `ci` run between them and `main`
sat unchecked for 85 minutes with every surface reading green (#5027). A
failing run is a **verified** commit and stays the canary's business; this
reports only the absence of an answer, and fails open at every unknown so it
can never be the thing blocking a repair.

A seventh, `main-red-hold.yml`, is the canary's other half: the canary *detects*,
and this is what consumes the detection at the point a merge is still a
decision. It runs on `pull_request`, asks the tracker whether a `main-red`
issue is open, and fails if one is — naming it. On 2026-08-19 the canary
worked exactly as designed and it did not help: it filed its issue at 16:57:01,
and four more PRs merged onto the non-compiling tree over the next 35 minutes,
the first of them **twelve seconds later** (#3917). Once `main` is red every
PR's checks are red too, so red stops distinguishing "your change is broken"
from "the base is" — which is how one composition break became four breaks in
three crates, each hiding the next. The `unblocks-main` label is the designed
way through, so the hold never blocks its own repair, and an unreachable
tracker fails **open** because this is the second line of defence and the
canary's issue is the first. Compiles nothing. `make main-red-hold` asks by
hand; `make main-red-hold-test` covers it, blocking branch included. Reporting
became holding on 2026-09-02, when `main is not known-broken` joined `main`'s
required status checks: a throwaway PR went red and unmergeable while a
hand-filed, `main-red`-labelled issue stood, and green again once
`unblocks-main` landed on it.

The chain has a third link, and it is not a workflow: **before you repair a
red `main`, check whether somebody already is.** On 2026-08-24 three sessions
each wrote the same two-line fix for the same break and merged them 95 seconds
apart (#4672, #4673, #4674). None was wrong — the hold means every session
with an open PR notices the red at once, and they all reach the same correct
conclusion. The canary's signal says `main` is broken; nothing said it was
being fixed. `scripts/main-red-claim.sh` is that second signal, the
`dispatch_claims` mechanic (#4300) with the tracker as the table:

```bash
./scripts/main-red-claim.sh check   # exit 0 proceed, 1 stand down
./scripts/main-red-claim.sh claim   # check, then post the claim
```

A claim is a comment, so it carries an author and a timestamp, and it lapses
after twenty minutes — an assignee would never lapse, and a crashed session
would then hold the repair of a red `main` shut. Every unknown proceeds
loudly: no `gh`, an unreachable tracker, an unreadable identity, two open
`main-red` issues at once. That direction is the whole safety argument, since
a claim check that can block a repair is worse than the duplication it
prevents. `make main-red-claim` asks by hand; `make main-red-claim-test`
covers it, standing-down branch included.

A claim carries a third fact, a session word, because one person runs several
agent sessions and the login cannot tell them apart. On 2026-09-02 three
sessions of one author each read the others' claims as their own, each
proceeded, and each opened a pull request splitting the same file the same
way, eight minutes apart. The word is `STELLA_CLAIM_SESSION` when a fleet sets
it, and otherwise a token kept in the clone's git dir, which answers per
worktree; `./scripts/main-red-claim.sh session` prints the one this clone
claims under. A run that has no word, and a claim comment written before the
word existed, both proceed on the old author-only rule, since an unknown here
proceeds like every other.

**The same collision happens over an issue, and `scripts/issue-claim.sh` is
the same mechanic pointed at one.** Two sessions implemented #5045 in
parallel and one merge kept one tree, dropping the other implementation with
no conflict to report; #4336 and #5054 are the same shape, three in one
afternoon (#5224). Every signal said that branch was abandoned — it existed
only locally, in a stale worktree, clean, with no remote branch and no PR.

```bash
./scripts/issue-claim.sh check 5045   # exit 0 proceed, 1 stand down
./scripts/issue-claim.sh claim 5045   # check, then post the claim
```

It asks the **pull requests first**, because that is the stronger signal and
the one the issue itself never shows: a sweep PR can close forty issues at
once while each of them still reads unassigned, unlabelled and open. Then the
claim comments, on the red-`main` rules — the tracker is the table so a peer
in another worktree can see it, a comment carries its author and timestamp, a
claim lapses so a crashed session cannot hold an issue shut, and every unknown
proceeds loudly. `make issue-claim N=5045` asks by hand; `make
issue-claim-test` covers it, both blocking branches included.

Run it **before writing code and again before opening the PR**. The gap
between those two is enough: a peer's PR can merge inside one issue's worth of
work, and then the check that was clean at the start is stale at the end.

An eighth, `windows-check.yml`, is the only compiler in this project that
looks at a `#[cfg(windows)]` arm: `ci.yml` runs on `ubuntu-latest` and
`release.yml`'s matrix is two Apple targets and two Linux ones, so every
non-unix body in the tree — `rootfd.rs`'s and `durable_write.rs`'s string
resolvers, and the Job Object half of `exec::GroupKillGuard` (#3550) — was
code no toolchain here or in CI ever parsed. It runs
`cargo clippy -p stella-tools -p stella-runtime` on a Windows runner, on the
paths that reach those two crates — the shipping code, deliberately not
`--all-targets`, because several `#[cfg(test)]` bodies import
`std::os::unix::fs::PermissionsExt` unconditionally and would fail the job on
a fixture rather than on the platform split. Those fixtures are #3497's
subject. It then **runs** two of them: `stella-runtime`'s `wrapper_socket` and
`wrapper_transport_limits`, which stopped being `/bin/sh` scripts when #3497
gave the crate a portable in-tree plugin binary
(`crates/stella-runtime/tests/fixtures/wrapper-plugin-fixture.rs`). That is the
Windows path being run rather than argued — the socket's stdio exchange, its
`env_clear()`, and the Job Object group kill #3550 added, which shipped with
"it compiles" as its whole evidence. A target list rather than the whole
suite, because the rest of it is still `#![cfg(unix)]` and a green over
nothing is worse than no green. Not a required check, and its own file rather
than a job in `ci.yml` for the reason `wire-schema.yml` has one — a Windows
runner is minutes a diff touching neither crate has no use for.

Its first run paid for itself: `git checkout` failed before the build, on
`crates/stella-cli/src/config/aux.rs` — `AUX` is a Windows device name, so
the repository was unclonable there and had been for as long as that module
existed. `reserved-paths` (`scripts/check-reserved-paths.sh`) is the fast
half of that finding, because a failed checkout reports one bad path per run
and there were two.

`windows-check.yml`'s other leg is `macos-check.yml`, same shape and the
same reason: `crates/stella-cli/Cargo.toml` carries a
`cfg(any(target_os = "macos", target_os = "windows"))` dependency table for
`cpal`, the microphone-capture crate `command_deck::voice` needs (ADR 0020),
and `ci.yml`'s `ubuntu-latest` job never resolves it. `release.yml` does
build macOS targets, but only on a tag push, well after a PR has already
merged — so a dependabot bump to `cpal` merged with `main` broken on macOS,
with nothing in the required check having ever compiled it. This job runs
`cargo check -p stella-cli --all-targets --locked` on `macos-14`,
path-triggered on `crates/stella-cli/**`, `Cargo.lock` and the workspace
`Cargo.toml`. Not a required check.

A ninth, `rebase-replay.yml`, is the only check here that reads a branch's
**history** rather than its tree, and it exists for a composition hazard the
shared-cell paragraph above does not cover: **rebasing a stale branch can
silently revert somebody else's change** (#4979).

Two branches make the same edit to one file — which a mechanical sweep split
across several pull requests does by construction. Branch B then undoes its
copy, so B's tree matches base and `git diff base -- <file>` is empty: B reads
as never having touched the file. Once A lands, rebasing B onto the new base
deduplicates B's edit (`warning: skipped previously applied commit`) and keeps
B's undo. The inert pair becomes a bare revert of A's change, B's diff for that
path goes from empty to a live revert, and it squash-merges cleanly. Merging B
*without* rebasing is safe, which is what makes this a rebase hazard rather
than a merge one — and this repository rebases constantly, because branches go
stale behind required checks. It happened to #4954 against #4951, and
`check-rebase-replay.sh` fires on #4939's merged head today.

**A path can drop out of the branch's diff for two reasons, and only one is
this hazard.** The branch removed it — an edit undone, or a file created and
deleted. Or the base branch renamed or deleted it and the branch absorbed that
through a merge, which is the ordinary way a long-lived branch survives a
refactor. Tree state cannot tell them apart: a file the branch created and
deleted is absent at the merge base and at the head, exactly like an inherited
rename. What differs is who removed it, so the guard asks that instead — a
suspect is dropped only when it is gone at the head *and* no non-merge commit
of the branch's own deleted it. An inherited removal is not in the range at
all, because merging the base advances the merge base past it. The record-plane
extraction is what forced this: it fired the guard on every open branch that
had correctly followed the move, and the printed remedy would have charged each
one a history rewrite for a hazard that was not there.

**The remedy is to flatten**, so the path is absent from the branch's history
rather than present as an edit and an undo. The branch's tree is identical
either way, so flattening costs nothing and the guard offers no
acknowledgement. `make rebase-replay` asks by hand;
`make rebase-replay-test` covers it, and reproduces the rebase itself so the
hazard is demonstrated rather than asserted. Its own workflow because ci.yml's
guards job checks out at `fetch-depth: 2` — right for `check-deleted-tests.sh`,
which compares two trees, and useless for a question about every commit on a
branch.

**A partition can also split something atomic**, which per-pull-request CI does
catch — each branch fails on its own — but only after the plan is already
wrong. `crates/stella-serve/src/accept.rs` and
`crates/stella-observatory/src/accept.rs` are byte-identical duplicates whose
`the_two_copies_of_this_policy_have_not_drifted` compares them with
`include_str!` from **both** crates, so a per-crate split of a repo-wide sweep
put one copy in #4951 and the other in #4954 and reddened both. Before
partitioning by crate, directory or owner, ask whether anything in the set has
to change atomically. `rg -l 'INTENTIONALLY DUPLICATED'` names today's only
such twin.

**Cite a document by its id, not its path.** Every document under `docs/` that
anything cites carries frontmatter with a stable `id`, and a citation names that
id — `doc:context-reuse §4`. Moving the file cannot break it. A document with no
`id` is deliberately not citable; `make doc-adopt DOC=…` gives it one. Legacy
path citations still work and repair themselves: `docs/manifest.json` records
id → path, so `make doc-links-fix` repoints them after a move. Anything outside
this repository is cited by URL. See `docs/README.md § How to cite a document`,
and `make doc-report` for what has gone stale.

This replaces two path-based guards (`check-normative-home.sh`,
`check-doc-citations.sh`) that were brittle in exactly the way their subject
was, and that only ever read Rust comments — 16 dead markdown-to-markdown links
had accumulated underneath them.

Four rungs, each a superset of the one above:

| Target | Runs | Honours `CARGO_SCOPE` |
| --- | --- | --- |
| `make guards-fast` | the toolchain-free guards + `lockfile-sync` + `fmt --check` — nothing compiles at all | — |
| `make guards` | ...plus `wire-schema`, whose two schema exporters do compile | — |
| `make check` | ...plus clippy — `lint` and `lint-schema`, both. No rustdoc at all | clippy |
| `make gate` | ...plus rustdoc and the tests — `doc-warnings` and `doc-warnings-schema`, both | clippy, rustdoc, test |

A `-schema` step and its base step move together across `check`. Both run
there, or neither does. That rule went unwritten, and it broke: `lint` ran
at `check`; `lint-schema` did not. So `GATE=fast git push`, and a hand-run
`make check`, never reached the three `schema`-gated crates. A broken
import there passed here and failed only in CI.
`schema-tier-parity` (`scripts/check-schema-tier-parity.sh`) now holds every
such pair to that rule. It reads `GATE_STEPS`/`CHECK_STEPS`, so a future pair
that splits the same way fails here first, not in CI. `doc-warnings-schema`
stays `gate`-only on purpose, paired with `doc-warnings`: at `check`, rustdoc
never runs, for either feature set.

`doc-warnings-schema` also wipes its own doc output before each run
(`cargo clean --doc`, for the three schema crates only). `cargo doc`'s own
freshness check can call a stale prior run "up to date" — and print nothing
— even after the source changed. That let a broken doc link in
`stella-plugin` pass a local `make doc-warnings-schema`, and show up only
after a hand-run `rm -rf target/doc`.

**Every rung needs `shellcheck` on `PATH`, and it is not vendored.** It is the
gate's one external binary, so a machine or container image without it stops at
that step on the lowest rung — `make guards-fast` included, despite compiling
nothing. The step refuses out loud rather than passing (`shellcheck:
UNAVAILABLE — THIS STEP DID NOT RUN`, #3615), because a lint that did not run
must not read as a lint that found nothing. `./scripts/setup-dev-env.sh --check`
reports it with the rest of the tooling, and the target itself names the install
command for each platform. #3830 is where "the image agents run in should carry
it" is tracked — that image is not defined in this repository, so nothing here
can install it.

`guards-fast` is not a rung you choose by hand; the pre-push hook picks it for
a push that reaches no crate *and* cannot have touched the wire contract — a
website-only or workflow-only push, which used to pay for a cargo build it had
no use for (#1439). `wire-schema` is conditional there, never dropped:
`docs/wire/` is generated and committed, so a hand-edit to it still takes the
dearer rung, and `.github/workflows/wire-schema.yml` covers the same paths
server-side because `ci.yml` ignores `docs/**`.

`CARGO_SCOPE` narrows only the compile tiers, and defaults to `--workspace`, so
`make gate` and CI are unchanged unless a caller asks for less (#1135):

```bash
make gate CARGO_SCOPE="-p stella-cli"   # that crate and its dependents
make impacted RANGE=origin/main..HEAD   # what the hook would pick for your branch
```

The global guards are never scoped: a 1500-line file or an unpinned action is a
fact about the repository, not about a crate.

`no-scratch` runs first because it costs milliseconds: it asserts no tracked
file matches a `.gitignore` rule. **Session scratch must never reach the
remote** (#448) — your reflections, plans, repro trees, and memory files stay
on your disk. Add the ignore rule *and* `git rm -r --cached` the path, because
git honours ignore patterns only for paths it is not already tracking. A
failure can also mean an ignore rule is too broad to accept new files; the
script's output tells you which case you're in.

**Run `make hooks` once per clone.** It installs a `pre-push` git hook
(`core.hooksPath=.githooks`) that runs `make gate` automatically on every push
and aborts the push if it fails. The point is *when* it fails: on your machine,
in thirty seconds, instead of an hour into `ci.yml` and a review round-trip.
It is advisory and per-clone (bypassable with `SKIP_GATE=1 git push` or
`git push --no-verify`), so it complements the required server-side checks
rather than replacing them. Before 2026-09-02, `enforce_admins` was off, so an
admin or auto-merge could still land gate-failing code, and the hook was the
only thing that caught that on the author's push. That gap is closed:
`enforce_admins` is on, an admin cannot merge past a red required check, and
branch protection is the backstop rather than the hook. `main-red-hold.yml`
now blocks the merge it once only reported, because `main is not
known-broken` and `gate steps no other workflow runs` both joined the
required contexts in the same change. It is also the only place some guards
run for long stretches:
`wire-schema` lived only in `make gate` until #1185 merged with stale generated
artifacts. When Actions is unavailable entirely (an org billing hold has
happened before — see RELEASING.md's local-release path), it is the only gate
running at all.

All of which holds only in a clone where `make hooks` has been run, and an
uninstalled hook announces itself in no way at all: `git push` simply runs
nothing and says nothing. So every rung of the ladder below ends by checking
`core.hooksPath` and printing a notice when it is not `.githooks`
(`scripts/check-hooks-installed.sh`) — silent when installed, and never a
failure, because it is a fact about the clone rather than about the change.
That is the cheap half of #3887 (#3932, #5587): the expensive half — whether
an admin merge past a red required check is permitted at all — is decided
now. `enforce_admins` is on, so it is not permitted, and branch protection
enforces that rather than any file in this tree.

The hook derives `CARGO_SCOPE` from the pushed diff via
`scripts/impacted-crates.sh`, so a change confined to one crate compiles and
tests that crate and its dependents rather than every member (#1135). It
widens to the whole workspace for a push to `main`, a tag, a diff touching a
workspace-root manifest / `Cargo.lock` / a build script / the gate machinery,
and for anything it cannot narrow with confidence. Two escape hatches sit
*above* `SKIP_GATE`, because the choice under time pressure should not be
binary:

```bash
GATE=fast git push       # make check — guards + fmt + clippy, no rustdoc, no tests
GATE=full git push       # the whole workspace, whatever the diff says
SKIP_GATE=1 git push     # nothing at all (emergencies)
```

`make impacted-test` covers the scoping rules; it is hermetic and deliberately
not part of `gate`.

The same diff also decides whether the hook runs `make bench-test` — the Python
bench suites, which are gated by `.github/workflows/bench.yml` and are not a
`make gate` step because they cost minutes. The hook selects on that workflow's
own scope filter, read from it by `scripts/bench-suites.sh filter` rather than
copied, so a push touching `bench/**`,
`crates/stella-model/src/catalog.rs` or the workflow itself runs them before it
leaves the machine. `GATE=fast` skips them out loud, on the same "no tests"
contract it applies to cargo. This existed nowhere until #2847: the workflow ran
seven pytest suites, `make bench-test` ran three, and `main` went red on
2026-08-11 on a deterministic failure in the arenabench suite (in-tree then,
ejected to its own repository since — #2380) with no local command that would
have caught it.

Supply-chain checks run as a separate CI job: `make supply-chain` (or
`cargo deny check advisories bans sources licenses`). All four are real
gates. (The CI job is still named "cargo deny + cargo audit" to match main's
branch-protection required check, even though cargo-audit itself was dropped
in #919 — cargo-deny's `unmaintained`/`yanked` settings are a strict superset
of what it added.) The license gate matters more than it looks: the workspace is
AGPL-3.0-only and dual-licensed, so a dependency carrying any further
restriction (non-commercial clause, field-of-use limit, or no license at all)
breaks both AGPL redistribution and the commercial track. **If `cargo deny`
rejects a new dependency, drop the dependency — do not widen the allow-list in
`deny.toml` without a licensing decision.**

---

## Architecture: ports, not direct dependencies

The central architectural invariant. Every design decision in the codebase
flows from this. If a PR breaks one of these, it will be asked to restructure
regardless of how good the feature is.

**This is the normative home.** The invariants are stated here and nowhere else;
`CONTRIBUTING.md` and `README.md` point at this section rather than restating it
(they used to carry their own copies, which had already drifted — one dropped
#8 entirely). **The numbering is an address:** Rust doc
comments, runtime error strings, and crate READMEs cite these by number, so
inserting or reordering an entry silently repoints every one of those citations.
Append; do not renumber. `scripts/check-invariants.sh` enforces both halves.

1. **Ports, not direct dependencies.** `stella-core` never imports a provider SDK, a
   filesystem API, or a terminal library. Models go through the `Provider`
   trait (`stella-protocol`), tools through `ToolExecutor` (`stella-core::ports`).
   A new vendor or tool is an adapter, never a rewrite.
2. **No I/O in the engine.** Decision logic (compaction, eviction, loop
   detection, budget, skill selection, hook matching) is plain synchronous
   functions over owned data inside `stella-core`. That's what makes it
   property-testable. Anything that spawns processes, reads files, or hits the
   network belongs in `stella-tools`, `stella-model`, `stella-cli`, or
   `stella-store` — injected as a port/trait, not called directly.
3. **Zero telemetry egress by default.** Community/default Stella sends no
   telemetry anywhere; model-provider traffic remains the normal network
   exception selected by the user. The sole additional egress is an explicitly
   enrolled Oxagen Enterprise managed mode: a signed org-managed document may
   authorize a minimal operational rollup to one exact allowlisted HTTPS sink,
   and only while the process-free execution authority is active. Prompts,
   paths, tool payloads/results, reasoning, errors, git state, memories, rules,
   and local identifiers are never exportable. Update checks and anonymous
   analytics remain prohibited.

   This is **enforced, not assumed** — `crates/stella-store/src/content_free.rs` holds
   the reviewed allowlist of hub `telemetry` columns and a sentinel harness
   every egress encoder registers with. Adding a hub column, or a key to an
   encoder, fails `make gate` until the allowlist is edited in the same PR, so
   a human has to answer "is this content?". A new encoder implements
   `ContentFreeEncoder` and joins `registered_encoders()`; an unbuilt drain
   format is a declared gap in `DRAIN_FORMATS`, not a silent omission. A leak
   here is a privacy incident, not a bug.
4. **Serde-first.** Every type crossing a crate boundary round-trips through
   `serde_json` byte-for-byte. Add a round-trip test when you add a type to
   `stella-protocol`.
5. **Typed errors, no panics.** Library code returns typed, named errors —
   never a bare `String`, never `.unwrap()`/`.expect()` on runtime data
   (network payloads, tool arguments, parsed source files are all runtime
   data). `unwrap` is fine in tests.

   **"Library code" is the literal thing: a crate that exposes a `src/lib.rs`.**
   `stella-cli` is a binary — it prints a message and exits, so a `String`
   there is the finished product, not an unnamed error. That was left implicit
   long enough to matter in both directions: an audit read all 421 of the CLI's
   `Result<_, String>` signatures as violations and reported a number five
   times the real one, while the genuine violations sat unchallenged because
   the rule looked hopeless to enforce.

   The test for whether a `String` is a defect is **whether a caller has to
   branch on it**. `crates/stella-runtime/src/error.rs` states the case, and had
   been quietly documenting this rule's own breach for as long as it existed: a
   session-create failure must tell a bad model slug (the caller's fault, a 400)
   from a misconfigured host (a 500), and a caller reaching that answer via
   `err.contains("model")` is parsing prose. Variants make the decision
   possible; a string makes it a guess.

   Enforced by `scripts/check-typed-errors.py` (`make typed-errors`), which
   fails the gate on a `pub fn` in a library crate returning
   `Result<_, String>`. It is a **down-only ratchet**
   (`scripts/typed-errors-baseline.txt`), for the one reason a ratchet is ever
   legitimate here: the rule predates the guard, so the baseline records a debt
   that already existed rather than granting new permission. It refuses to
   raise a count — `make typed-errors-update` errors rather than write a bigger
   number — so the only way past it is to type the signature. It is meant to
   reach empty; the remaining 43 conversions are tracked in #2392, and adding a
   crate to that file to turn the gate green is the expedient CLAUDE.md forbids.

   Internal (`pub(crate)` and private) helpers are deliberately **not** covered:
   the hazard is a caller that cannot branch, and those have no callers outside
   the crate that wraps them. Widening the guard to them is a separate
   judgement, not an oversight.
6. **Budget aborts at safe boundaries only** — never mid-tool. `run_turn`
   consults the budget guard only between model calls, never interrupts a
   tool in flight.
7. **Byte-stable prompts.** Anything that feeds the system prompt must be
   deterministic — prompt-cache hits are a feature, and nondeterminism there
   is a cost regression. Memories are loaded once per session and concatenated
   in sorted filename order; recalled context rides as a volatile message
   *after* the stable prefix (see `crates/stella-cli/src/agent/prompt.rs::build_system_prompt`
   and `crates/stella-cli/src/memory.rs` for the L-E8 discipline).
8. **Provider feature parity is declared, not assumed.** Providers diverge
   in sneaky ways, and this is guarded on **six axes** today in
   `crates/stella-model/src/provider_parity.rs`:
   - **`CachePosture`** — how the prompt cache is engaged/observed
     (Anthropic's cache is explicit opt-in; DeepSeek spells its cache-hit
     telemetry differently; OpenRouter needs a request-root `cache_control`
     plus a sticky `session_id`).
   - **`ReasoningPosture`** — how reasoning/thinking is controlled on the
     wire (`Controllable`/`FixedOn`/`FixedOff`/`Unsupported`). Only Z.ai
     (`thinking`), OpenRouter (`reasoning`), Anthropic/Gemini/Vertex
     (thinking budget / `thinkingLevel`), OpenAI and now xAI
     (`reasoning[_]effort`) honor a pinned effort; the shared adapter drops
     it for `Unsupported` providers (bedrock/deepseek/local) — and a pinned
     effort against one surfaces a one-line boot notice, never a silent drop.
   - **`StreamFallbackPosture`** — how a provider recovers when its
     streaming path is broken (a stream hung before its first byte, or a
     200 with an empty stream). Every streaming dialect arms a bounded
     per-session latch and re-issues the retried attempt as a unary request
     — the shared chat-completions adapter first (#2686), then Messages,
     Responses and `generateContent` (#2746). Bedrock is already unary, and
     that row names a witness like every other (#4557): "no stream to fall
     back from" holds only while the single unary path classifies its own
     read-bound expiry as terminal rather than as a retryable transport
     fault, which is #547's retry storm.
   - **`OverflowPosture`** — whether this provider's context-overflow
     rejection is recognised as one, so the engine's reactive recovery
     fires instead of aborting the turn (#2680). `Detected` names the wire
     signature *and* the test proving that exact body shape classifies as
     `ContextOverflow` (Anthropic's `prompt is too long: N tokens > M
     maximum`, OpenAI's `context_length_exceeded`, …). `BestEffort` is a
     declared gap, not a silence: errors still funnel through the shared
     classifier, so an overflow phrased in a detected dialect is caught
     opportunistically and anything else degrades to a safe unrecovered
     abort. Verifying the real wire shape upgrades the row.
   - **`OutputBudgetPosture`** — whether this provider refuses the
     *requested output ceiling* rather than billing what is spent, and
     whether that refusal is recognised, so the engine clamps
     `max_output_tokens` and re-asks instead of aborting the turn. It
     diverges hardest at the gateways: a gateway prices the request against
     the ceiling the caller asks for, so OpenRouter refuses a 128K ask
     (`can only afford M`) against a balance that would fund the real call
     several times over. `Detected` today for `openrouter` alone, witnessed
     on its exact recorded body; every direct vendor is a declared
     `BestEffort` gap. Unrecognised, this failure killed three benchmark
     runs outright.
   - **`ParallelToolCallPosture`** — whether several tool calls ride one
     assistant message (#4163). The engine dispatches consecutive read-only
     calls concurrently and the system prompt asks the model to use that
     ("send independent tool calls together"), so both halves of the working
     surface already depend on this — and nothing declared it or tested it.
     Alone among the axes it records **two** facts, because they have
     different kinds of evidence and collapsing them would let a row claim
     more than it can show: `ParallelAdmission` is about a vendor's live API
     and can only be settled by *observing* a run, while `fan_in_witness` is
     about this tree's own adapters and is settled by a wiremock test — which
     every row names, gap rows included. `DefaultOn` today for `openrouter`
     and `zai`, each citing a census of this repository's own store.db
     (grouping `tool_start` events by the preceding `step_manifest`); the
     other eight are `Undetermined` rather than assumed to match a sibling
     dialect. The measurement is also what decided the fix: both settled
     routes already emit parallel calls in volume with **no opt-in ever
     sent**, so no `parallel_tool_calls` request field was added.

   Each provider id declares a posture on **every** axis and, for a
   controllable/opt-in/implicit/fallback posture, names the **witness test**
   proving it on the wire. Tests enforce each matrix from both sides: `stella-cli`'s
   config tests fail if a seeded provider lacks a row on any axis, and
   `stella-model`'s parity tests fail if a row's witness test no longer
   exists. Adding a provider — or a new divergent feature axis — means
   updating the matrix in the same PR. Born from a real defect: OpenRouter
   ran Claude models with ZERO prompt caching for months because nothing
   enforced the cache axis; the reasoning axis was added after the same
   silent-drop shape recurred for pinned `effort`.
9. **Tool-first, single-purpose.** Every capability the agent has is a tool,
   and a tool does exactly one thing. A parameter may *scope* the operation
   (a key, a path, an offset); it may never *select* the operation —
   `update_task(delete=true)` is two tools wearing one schema and gets split
   (`edit_task` + `delete_task`). Enforced at review for two reasons: the
   model decides whether to call a tool on `ToolSchema::description` alone,
   and a two-verb tool teaches neither verb well; and per-tool policy
   (`tools.<name>` toggles, the `command.started` gate) must be able to
   withhold the destructive verb without withholding the benign one. A read
   tool with a mutating arm also cannot declare `read_only` honestly, which
   corrupts the engine's concurrency contract. The scratch state plane
   (`save_state` / `get_state` / `list_state` / `delete_state`) is the
   reference shape.
10. **Every emitted signal names its consumer.** An `AgentEvent` variant that
   nothing reads is allowed, but only as a **declared, issue-cited gap** —
   never as a silence nobody noticed. The ledger is
   `crates/stella-protocol/src/event/consumers.rs`: one row per wire tag, each
   declaring a `ConsumerPosture` (`Behavioral` names the code that branches on
   it, `Surfaced` names the surfaces that select it, `RecordedOnly` and
   `Unclassified` each cite the issue where the gap is being decided). The
   rows are generated from the same table as `KNOWN_TYPE_TAGS` and
   `type_tag()` (`crates/stella-protocol/src/event/tags.rs`), so adding a
   variant without declaring what consumes it is an `E0004` — a build error,
   not a red test (#2730). `MAX_UNCLASSIFIED` is a down-only ratchet on the
   unaudited backlog.

   Born from a repeated real defect, every instance of which was found by a
   bench run paying for it rather than by a test: `flip.json` written with
   nothing reading it (#1536); `verify_done` confirmations tallied but not
   feeding the halt, which cost `solved_then_timeout` four times on one
   certification panel before #2661 wired it; the flip transition still
   emitting nothing durable, so a shipped halt cannot be measured in the
   field. This is invariant #3's discipline pointed at consumption instead of
   egress — a reviewed table plus enforcement from both sides (#2701).

   What it does **not** prove: that a `Behavioral` row's `site` still points
   at live code. That string is prose for a reviewer. The enforced half is
   totality (by the compiler) plus issue citation and posture coherence (by
   test) — enough to make a PR author answer "what reads this?" before the
   merge.
11. **Every way Stella changes itself is declared.** Stella evolves through
   several object classes — framework, memory, skill, tool, workflow, model —
   and each one used to answer "when may this change, on what evidence,
   published by whom, and how is it undone?" in a different file or nowhere.
   The ledger is
   `crates/stella-parity/src/evolution.rs`: one row per surface, declaring a
   posture, a timing, an impact class, a rollback artifact, and — for a live
   posture — the witness test proving the surface can actually be changed.
   `EvolutionSurface` and its rows are generated from one macro table, so a
   surface with no row does not compile.

   The `evidence` and `authority` columns are **not stored**. A row declares
   what a wrong change to it can break (`ImpactClass`), and the required
   provenance grade and publication authority are read out of
   `crates/stella-protocol/src/provenance.rs`. One policy, read in two places,
   with no second copy to drift (#2780, #2782).

   That policy is the other half of this rule, and it says that
   **aggregation never promotes evidence**. N model critiques agreeing remain a
   model critique; only re-deriving a claim against a stronger source moves it.
   A prompt hint may be trialled from a mined trajectory, and a blocking guard
   or an executable tool needs deterministic proof plus a person — because
   those two break a teammate's session, and blast radius is what the grade
   rations. #2569/#2570 is why vote-counting is not an alternative: ablating
   the verifier turned every run into a PASS and the count never noticed.

   What it does **not** prove: that a live path actually asks the gate before
   publishing. The ledger declares the terms and the policy can answer them;
   wiring `authorises` into each publication path is tracked separately, and
   the rows say plainly where today's evidence falls short of what they
   require.
12. **`stella-core` holds the step path.** A module that lives there is one the
   engine reaches — from `driver`, `step` or `ports`, transitively, through
   paths it actually names. Anything else is in the crate because somebody put
   it there, and belongs in its own.

   Reachability from the **crate root** is not this: `search.rs` is reachable
   from `lib.rs`, which is why `module-reachability` passes on it, and how a
   15k-LOC subsystem lands here behind a `pub mod` line. That is how the
   record plane arrived, and it has since left for
   `stella-records` (#5113, #5117); the learning plane left for
   `stella-learn` the same way.

   A `#[cfg(test)]` reference does **not** count. A subsystem the engine
   touches only from its own tests is a subsystem the engine does not need —
   that is what made the whole skill catalog look like engine code while the
   step path only ever called the invocation vocabulary beside it.

   Enforced by `scripts/check-core-reachability.py` (`make core-reachability`),
   a **down-only ratchet** over `scripts/core-reachability-baseline.txt`. The
   baseline records the residents that predate the guard, so it lands green;
   `--update` refuses to add an entry, so a red run is cleared by moving the
   module or by making the engine genuinely use it, never by recording it. It
   is meant to reach empty, and each eviction under #5113 deletes a line.
---

## The definition of done: witness tests

This repository holds every contribution to its own gate: a PR ships a test
that **fails on the old code and passes on the new**. That is
`CONTRIBUTING.md`'s contract for a change to *this* repo, and it holds
regardless of anything below — it is not what base Stella promises a user.

For a behavior change or feature, a PR should include a **witness test**:

- It **fails** on `main` without your change (the feature is genuinely absent).
- It **passes** with your change (the feature is genuinely present).

Check it the artisanal way (`git stash && cargo test -p <crate>`). Pure
refactors, docs, and CI changes don't need a witness — say so in the PR
template. If a witness is genuinely impractical (e.g. TUI rendering), explain
how you verified the change instead.

**What follows described the staged pipeline's own witness/verify machinery,
a different thing from the contract above — the built-in path this section
described has been deleted from this workspace (#3865, the last slice of
`docs/spec/pipeline-as-plugins.md` §7's extraction plan; see this branch's
history for the deletion itself). It is kept here, rewritten to the
post-removal state, because the *shape* it names — a demand-driven witness
stage, a flip oracle, a terminal verify ladder — is exactly what
`doc:pipeline-as-plugins` §8 ports into Vera, Oxagen's private reference
verification plugin, rather than reinventing.** Host-run verification is no
longer something Stella ships in-tree: `stella run --pipeline classic` is
refused outright (`crates/stella-cli/src/wrapper_plugin.rs`'s
`PipelineChoice::resolve`), and the three verification flags split two ways
in the same file's `reject_verification_flags_without_pipeline`:
`--keep-witness` and `--require-verified` are refused on every resolution,
because what they asked for was host-run machinery that no longer exists
here; `--test-command` is refused on the raw default and **passed through**
under `--pipeline <variant>`, where it hands the bound plugin's own
`[oracle]` a command to check against. Each refusal names `stella plugin
install` — an installed verification plugin — as the remedy rather than a
flag this repository ships. When such a
plugin is installed, `stella run --pipeline <plugin-id>` hands it the turn;
the plugin's own `[oracle]` runs the check and reports its evidence, which
Stella evaluates against the plugin's declared verdict rule and does not
re-run or re-check (see AGENTS.md's opening). What used to be host-run
mechanics — an independent model (never the worker) authoring the failing
witness test, tracking its fail→pass flip in a flip oracle, refusing to
credit the flip if the worker modified the witness files (tamper exclusion),
authored on demand once the warrant found something worth proving, ordered
by a `[wrapper]` manifest rather than hardcoded — is now the shape a
verification plugin implements on its own side of the wrapper socket, not
code this repository runs. `stella-cli`'s surviving glue for that surface
(the candidate-grant/tamper-watch minting logic a witness-flavoured plugin's
flip crediting depends on) lives in `crates/stella-plugin/src/candidate_grant.rs`
now, not in a deleted `stella-pipeline`.

**Verification itself buys no model call, and that constraint transfers to
the plugin path unchanged.** The witness *author* is the one verifier-tier
call a verification plugin spends, and it survives because it creates the
oracle rather than substituting for one — its test either goes fail→pass or
does not, and that is decided by running it. Everything downstream of a
plugin's evidence is meant to stay deterministic on the plugin's own side:
`ladder_decision`'s terminality — no arm escalates to a model — was the
built-in pipeline's own invariant (`LadderDecision`'s doc comment, historical
now that the enum shipped in the deleted crate) and is exactly what
`doc:pipeline-as-plugins` §8 asks Vera to preserve as it ports the logic
rather than copies it. This file deliberately does not restate a variant
count for a type it no longer hosts, because a number copied into a second
file drifts — that happened once already while `LadderDecision` still lived
here (#3473) and is the reason to name the risk rather than repeat the
number. The model verdict and the distress-guidance call are gone (#2584),
structurally rather than by default — `Roster::apply` rejects both keys as
`NotAssignable`, so no configuration restores them; this holds independent
of the deletion above. See `website/content/docs/inference-pipeline.mdx` for
the historical stage flow and the plugin path that replaces it.

**A witness that no longer exists cannot fail.** Three times a PR has landed
that silently deleted a test another PR added to the same file hours earlier —
both branches green, the merge textually clean, git with no conflict to report
because one side simply does not contain the other's lines (#1976, and the same
shape in #1860). `check-deleted-tests` runs in CI on `pull_request` only,
because it is the one question here about *two* trees: it compares the base
branch tip against the merge result and names any `#[test]`/`#[tokio::test]`
that did not survive. Deleting a test is not forbidden — renames and deliberate
removals are ordinary — it just has to be **named in the PR description**, which
turns an invisible deletion into a sentence a reviewer reads. It is deliberately
not a `make gate` step: locally there is no second tree to compare.

---

## Nothing left behind — every finding becomes a fix or a GitHub issue

The standing rule for every session, human or agent, and the companion to the
witness-test contract above: work is not finished while anything you noticed
lives only in your head, a chat transcript, or a worktree that is about to be
deleted.

- **Fix what you can inside the change you are already making.** A bug you can
  fix safely within your current scope gets fixed, not filed.
- **Everything else becomes a GitHub issue before you finish** — a bug you saw
  and did not fix, a defect you worked around, a missing test, an idea worth
  keeping, dead or unwired code you noticed, and the logical next step of the
  work you just completed. If your change ships scaffolding that something else
  must later wire up, file the issue for that wiring in the same PR — unwired
  code with no tracking issue is exactly the failure mode this rule exists to
  prevent.
- **Write every issue as a handoff.** Assume the reader is a fresh agent with
  none of your session's context: state the problem, the files involved (paths,
  not descriptions), how to reproduce or verify, the constraints you already
  discovered (gates, invariants, related PRs and issues), and what "done" looks
  like. A one-line issue that needs your memory to interpret is a note to
  yourself, not a handoff.
- **Search before filing** (`gh issue list`, `gh search issues`) and link
  related issues instead of duplicating them. Reference the issues you filed
  from your PR description so the residue of the work is auditable.

The judgment half of this rule — did you notice something and not file it —
is not mechanically decidable, and the PR template asks a human. Its most
common *residue* is checked: `left-behind` (`scripts/check-left-behind.sh`)
fails the gate on a `TODO`/`FIXME`/`XXX`/`HACK` in code that names no issue,
because a marker with no `#1234` beside it is by definition a thing left
behind with no handoff (#1454). A marker that names an issue is tracked work
and passes. The fix is always to file the issue and reference it — never to
delete the marker, and never to add a baseline entry: that baseline started
empty and is meant to stay empty.

---

## Workspace layout — where a change goes

Twenty-seven crates, every one under the `crates/` directory (`crates/stella-core`,
`crates/stella-cli`, …; the two bench members stay under `bench/`). The
one-sentence rule of thumb below routes you to the right one; **each crate's
own `README.md`** (linked from the table) then covers its boundary, layout,
invariants, gotchas, and extension recipe in depth. Read that before changing
code inside a crate you don't already know — its "Boundary" section answers
whether your change belongs there at all, and its "God files" section names
the files you must plan around (see below).

| You want to… | Crate | Notes |
|---|---|---|
| Change the agent loop (plan / retry / compact / budget / loop-detect / hooks) | [`stella-core`](crates/stella-core/README.md) | **No I/O allowed.** Decision logic only. Skills and rules live in `stella-learn`. |
| Add/fix a model provider (SSE, tool-call dialect, pricing) | [`stella-model`](crates/stella-model/README.md) | One file per adapter (`anthropic.rs`, `openai.rs`, `gemini.rs`, `vertex.rs`, `bedrock.rs`, `zai.rs`). Copy an existing adapter's shape. |
| Add/fix a built-in tool (`bash`, `read_file`, `edit_file`, `search`, `task_create`, `save_state`, `get_environment`, …) | [`stella-tools`](crates/stella-tools/README.md) | Implement the `Tool` trait, register in `ToolRegistry`, declare one line in `catalog.rs`. |
| Change CLI commands, flags, or agent wiring | [`stella-cli`](crates/stella-cli/README.md) | This is the shipping binary. |
| Change REPL rendering / panels / keybindings | [`stella-tui`](crates/stella-tui/README.md) | Pure-fold ratatui REPL — the Command Deck, the default interactive shell on a TTY. The v2 redesign (`design/tui-v2/SPEC.md`) landed **in place**: its surfaces are `src/views/`, its palette is the `stella-tui-theme` crate, and `src/palette.rs` re-points the v1 names at v2 tokens. There is no `src/v2/` directory — an earlier plan for one was dissolved. |
| Change a **v2 colour, state glyph, or the wordmark** | [`stella-tui-theme`](crates/stella-tui-theme/README.md) | **A near-leaf: `ratatui` is its only dependency**, so every v2 surface can take it without cost. The v2 palette plus the hue clamp that holds it — gold must clear `g >= 0.78 r` or it is orange, grays must be neutral or blue-tipped or the scheme reads sepia — enforced as unit tests on the shipped table. Deliberately **not** a superset of `stella-tui::palette`: v1 is warm-neutral by design and v2's clamp rejects a warm gray outright, so the two coexist until each surface migrates. |
| Touch shared types crossing a crate boundary | [`stella-protocol`](crates/stella-protocol/README.md) | **Zero logic, zero I/O — types only.** |
| Resolve where `~/.stella` is — home dir, stella home, the user-tier data dir | [`stella-home`](crates/stella-home/README.md) | **A leaf with NO dependencies at all**, which is what lets `stella-store`, `stella-observatory`, `stella-cli`, `stella-model` and `stella-tools` all share it (the observatory must not link the store). Every resolver has a pure `resolve_*` half that reads no environment. |
| Parse/validate a plugin's manifest, or change the wrapper socket's **wire contract** — the request/response types a non-Rust plugin speaks (`before_turn`/`after_turn`, `EvidenceSet`, `VerdictRule`) | [`stella-plugin`](crates/stella-plugin/README.md) | **Near-leaf: `stella-protocol` is its only workspace dependency** (#3245 slice A; #3310) — pure parsing/validation over borrowed text, plus `src/wire.rs`'s serialized shapes (#3380, `doc:wrapper-socket` §2). The engine never learns plugins exist: the host binds these grants to the engine's gates, and `stella-core` must never depend on it. The one edge it does take is what lets it share `HookEvent` with the engine instead of mirroring it by hand. |
| Change the wrapper socket's **trait** — `TurnWrapper`, `admissible`, `judge`/`again`, the in-process/subprocess transports | [`stella-runtime`](crates/stella-runtime/README.md) | `src/wrapper/` (#3380, landed #3479, `doc:wrapper-socket`). Lives one layer above `stella-core` because `before_turn`/`after_turn` do I/O, which invariant 2 bans in the engine; consumes `stella-plugin`'s wire types rather than redefining them. No live turn dispatches through it yet — see the crate's own README. |
| Change how a plugin is **installed, listed, removed, or trusted** — `.stella/plugins/` and `~/.stella/plugins/` resolution, install consent, the project-tier trust gate | [`stella-cli`](crates/stella-cli/README.md) | `src/plugin_cmd.rs` + `src/plugin_cmd/{roster,process}.rs` — renders `stella_plugin::consent_text` before anything executes, gates a cloned repository's plugins on `project_code_execution_trusted()` (#3509), and is the one place `LoopGrant::permits_hook`/`permits_point` get consulted against an installed manifest. |
| Decide whether a human is present to see/answer a mid-run prompt | [`stella-tty`](crates/stella-tty/README.md) | **A leaf with NO dependencies at all** (#3036) — one pure `human_can_answer(interactive_output, stdin_is_terminal, prompt_is_visible)`, which is what lets `stella-cli`'s approval prompts and `stella-model`'s credential prompt share one derivation without `stella-model` depending on `stella-cli` (invariant 1). |
| Emit a diagnostic — a record explaining *why* the program did something | [`stella-diag`](crates/stella-diag/README.md) | **A leaf: `serde` only, so anything may depend on it.** Field values cannot hold a `String`, a `Path`, or model output — that is a compile error, not a review question. Design: [`docs/spec/diagnostics.md`](docs/spec/diagnostics.md). |
| Compute a line-oriented unified diff (`@@` hunks, git's exact shape) | [`stella-diff`](crates/stella-diff/README.md) | **A leaf with NO dependencies at all** (#1511) — pure functions over borrowed strings, which is what lets [`stella-observatory`](crates/stella-observatory/README.md) and [`stella-cli`](crates/stella-cli/README.md) share one differ without costing the observatory its isolation. |
| Render a session transcript — folds, digests, diffs, chips — on the web or a character grid | [`stella-transcript`](crates/stella-transcript/README.md) | **Near-leaf: [`stella-diff`](crates/stella-diff/README.md) is its only workspace dependency.** One information model, two renderers, no I/O. Both surfaces render from the same folds and the same diff rows — the Observatory used to re-implement the TUI's painter in JavaScript, and that copy had drifted to having no diff rendering at all. |
| Turn text into a vector, or compare two vectors honestly | [`stella-embed`](crates/stella-embed/README.md) | **A leaf with NO workspace-crate dependencies** — the `Embedder` seam, the fingerprint every stored vector is stamped with, the `SimilarityPosture` a backend must declare, and a pure deterministic ranker. Shared by [`stella-context`](crates/stella-context/README.md) (retrieval) and [`stella-graph`](crates/stella-graph/README.md) (semantic code search) so neither has to depend on the other. |
| Change the record plane — the typed record taxonomy, the ingestion boundary, or the registry that merges markdown rules and TOML records | [`stella-records`](crates/stella-records/README.md) | Pure value logic, no I/O. It left `stella-core` because the engine reached the whole plane through one hash call, which now goes to `stella_protocol::hash::record_hash`. It takes the markdown rule parser and the redactor from `stella-learn`, and still depends on `stella-core` for the steering candidate types `adapt` maps onto; the re-layering epic tracks inverting that last edge. |
| Change what the agent learns and what steers it — skills, rules, the miner behind both, secret redaction, A/B comparison, the significance test | [`stella-learn`](crates/stella-learn/README.md) | **Near-leaf: `stella-protocol` is its only workspace dependency.** Pure value logic, no I/O; `RuleSource` and `SkillSource` are the ports, implemented in `stella-cli`. It left `stella-core` because the engine reached only the skill-invocation vocabulary, which stayed behind as `stella_core::skill_invocation`. |
| Persistence: executions, events, telemetry (SQLite) | [`stella-store`](crates/stella-store/README.md) | |
| Retrieval: graph, embeddings, episodic memory | [`stella-context`](crates/stella-context/README.md) | |
| Tree-sitter code indexing | [`stella-graph`](crates/stella-graph/README.md) | |
| MCP client (external tool servers) | [`stella-mcp`](crates/stella-mcp/README.md) | |
| Multi-agent fan-out, worktree isolation | [`stella-fleet`](crates/stella-fleet/README.md) | |
| Change the self-driving loop's decision math (AIMD cycle sizing, the aperture ladder, the dry-streak oracle, the finding-dedup digest, the `runs.jsonl` fold) | [`stella-autonomy`](crates/stella-autonomy/README.md) | **A leaf with no workspace-crate dependencies** (`serde`/`serde_json`/`sha2` only), shared by [`stella-cli`](crates/stella-cli/README.md)'s self-driving verbs and [`stella-observatory`](crates/stella-observatory/README.md)'s dashboard so the two cannot drift (#1613) — the observatory must not link `stella-core`. |
| The Observatory telemetry dashboard (`stella observe`) | [`stella-observatory`](crates/stella-observatory/README.md) | Loopback-only, read-only, embedded HTML. |
| The headless engine server a host process drives over the wire | [`stella-serve`](crates/stella-serve/README.md) | Its **own binary**, not linked into [`stella-cli`](crates/stella-cli/README.md). Every model/tool call is remoted back to the host; the engine holds no ambient authority. Design: [`docs/spec/serve-surface.md`](docs/spec/serve-surface.md). |
| Drive the engine one step at a time from a durable host (checkpoint/resume) | [`stella-engine`](crates/stella-engine/README.md) | Re-export-only facade over `stella-core`'s step loop (#971); no logic, no I/O. Consumed by [`stella-serve`](crates/stella-serve/README.md) and external hosts — `stella-cli` does not link it. |
| Share the engine-assembly bottom half (provider, registry, store, budget), or the wrapper socket built on top of it | [`stella-runtime`](crates/stella-runtime/README.md) | `RuntimeSpec` → `RuntimeBuilder` → `SessionRuntime`, construction only. Reads no ambient environment by contract (`tests/no_ambient_reads.rs`). Also owns the wrapper socket — see the dedicated row above. |
| Declare CLI-vs-API capability parity (witnessed, ratcheted) | [`stella-parity`](crates/stella-parity/README.md) | The cross-surface capability matrix: every engine capability carries a posture + named witness test per surface, so a feature cannot ship on one surface and silently miss the other. |
| Context Graph Protocol (wire types / host / conformance) | external repo: [`context-graph-protocol`](https://github.com/macanderson/context-graph-protocol) | Split out of this workspace; Stella depends on it as registry crates (`contextgraph-*`) pinned with exact `=` version requirements in the root `[workspace.dependencies]`. Stays dependency-light by contract. |

### When a new crate is justified

**This is the normative statement of the rule.** Six crate READMEs used to
carry their own paraphrase of it, each worded differently, with nothing for
them to point at (#3721); they now cite this section and keep only their own
answer to it. Cite it as *AGENTS.md § "When a new crate is justified"*.

A new crate is warranted only when functionality (a) sits behind a port/trait
and would otherwise drag heavy dependencies into a crate that is deliberately
light, (b) needs a dependency direction the current graph forbids, or (c) is a
genuinely separate deliverable with its own binary and release cadence.

Absent all three, extend an existing crate. A new one costs a row in the table
above, an impacted-crates scope, CI time and a README, and a wrong split is
harder to undo than a wrong merge. A justified one updates that table and the
root `Cargo.toml` members list in the same PR.

### God files — plan around them, never into them

Read this **before** planning any change, because it constrains where new
lines may land. The gate's `file-size` guard (`scripts/check-file-size.sh`)
enforces a 1500-line ratchet with a grandfather list
(`scripts/file-size-baseline.txt`). Two rules follow, and both are planning
inputs, not review afterthoughts:

- **No new god file can exist.** A new file that crosses 1500 lines fails the
  gate outright, and the baseline accepts no new entries — a file approaching
  the limit gets split, not grown over it.
- **The grandfathered god files below are closed to growth: do not add lines
  to them.** Plan work so new logic lands in a sibling submodule instead
  (`crates/stella-core/src/driver/settlement.rs`, split out of `driver.rs`,
  is the pattern). A ceiling moves only via `make file-size-update`, which
  lands as a reviewable baseline diff to be justified like any other change —
  an escape hatch for a genuinely irreducible line (a module declaration in
  an already-oversized `lib.rs`), never something a plan may assume. It moves
  a ceiling and nothing else: `--update` **refuses to add** an entry for a file
  that is not already in the baseline, naming it and pointing at the split
  remedy (#3441). Before that it recorded every over-limit file it could see,
  so a run clearing an unrelated ceiling drift would quietly grandfather
  whatever first-time crossing happened to be sitting in the tree — inside a
  diff whose stated purpose was the bump, and green forever after. If a
  crossing is genuinely irreducible, say so out loud with
  `./scripts/check-file-size.sh --update --grandfather <path>` and justify it
  in review.
- **`make file-size-update` raises; `make file-size-retighten` lowers** (#4657).
  The update moves the ceilings that must move for your tree to pass and leaves
  every other entry at the number it had, so a PR repairing a red `main` edits
  exactly the lines it needs. It used to lower them all to their size on the
  branch it ran on, which made the repair PR the carrier of the next break: it
  is the one PR guaranteed to be racing every other merge, because main is red
  and everyone is waiting on it. #4652 repaired `driver.rs` and `usage.rs` and
  in the same run lowered `command_deck.rs` from 3752 to 3656 against a main
  whose copy was 3737; that is #4654, and it held every open PR for half an
  hour. Reclaiming the slack a split earned is `file-size-retighten`, and it is
  a PR of its own — safe precisely when nothing is blocked on it. Both modes
  still retire an entry whose file dropped under the limit or is gone, because
  the check hard-fails on those and names the update as the only remedy.
- **A passing run names the files nearing the line** (#4897). `check-file-size`
  used to say nothing about a file until the moment it failed, so a file eight
  lines under its ceiling and a file at four hundred produced the same green
  line — and the author who then met the ceiling was the one whose change was
  about something else and who had no room to design a seam. The run now lists
  the crowded files, tightest first, and exits 0 either way: it reports, it does
  not judge. Grandfathered files are left out, because a baseline entry sits at
  its ceiling by construction and is already named here and in its crate README.

**The ratchet judges your change, not the tree** (#2004). A file over the line
fails only when `current > max(its own limit, size in the base tree)` — where
that limit is the baseline entry for a grandfathered file and the 1500 lines
above for every other (#2397). So a violation that was already there before
your branch existed is reported as drift and does not fail you, while anything
your change grows past what it inherited fails exactly as it always did.
Inherited drift is survived, never absorbed: a first-time crossing you walked
past still wants splitting, and still gets no baseline entry. The base is the
merge commit's first parent on a pull request, the merge base locally, and
nothing at all in a shallow clone or a fresh repository — where the guard falls
back to the strict whole-tree check, because an unresolvable base must make it
stricter, never weaker. This exists because the baseline is one shared cell
every growing PR must write, and three times running two PRs that each wrote it
correctly composed into a red `main` that then failed everyone downstream. Two
consequences for planning: **do not "fix" a red ratchet you did not cause** by
folding a baseline regeneration into an unrelated PR — land it on its own from
a fresh `main`; and note that splitting a god file buys structure, not slack
until someone runs `make file-size-retighten`, which is a separate PR.

The workspace's Rust god files, by crate (the bench harness's Python offenders
sit in the same baseline). Each file's ceiling lives in
`scripts/file-size-baseline.txt` and is deliberately not repeated here: that
file is generated and gate-enforced, so it is the only copy that can stay
correct. This table names *which* files are closed to growth, which is the part
a plan needs and the part that rarely changes:

| Crate | God files |
|---|---|
| `stella-cli` | `src/command_deck.rs` |
| `stella-core` | `src/driver/tests.rs`, `src/driver.rs`, `src/bus.rs` |
| `stella-model` | `src/zai/tests.rs`, `src/anthropic/tests.rs` |
| `stella-store` | `src/tests.rs`, `src/lib.rs`, `src/usage.rs` |
| `stella-tui` | `src/deck_ui.rs` |

The other twenty-two crates carry no god files — keep it that way. Each crate's
README repeats its own list under "God files — do not add lines", so the
constraint is in view wherever planning starts.

All three copies — this table, those README lists, and each clean crate's "no
god files" claim — are checked against `scripts/file-size-baseline.txt` by
`god-files` (`scripts/check-god-files.sh`). `make file-size-update` rewrites the
baseline and touches no prose, so before that guard existed the next split or
rename stranded every copy silently (#1435). The baseline is the tiebreaker: it
is generated and gate-enforced, so the prose follows it and never the reverse.
Only *which* files are named is checked — the ceilings stay in the baseline
alone, because a number in two places is how the last limit died.

**Status — what ships.** The live runtime path is
`stella-cli` → `stella-core` → `stella-model` / `stella-tools` / `stella-store` /
`stella-context` (recall only) / `stella-mcp`, and the CLI also drives
`stella-fleet` (`stella fleet`) and `stella-tui` (the Command Deck, the
default interactive shell on a TTY). Verification is opt-in on every door via
`stella run --pipeline <variant>`, naming an installed wrapper plugin — the
built-in staged pipeline this flag used to be able to name (`classic`) has
been deleted from the workspace (#3865) and is refused outright; the raw
step-loop is the default with or without the flag. The fuller
`stella-graph` retrieval + context plane (`stella init` builds the code-graph
index; recall fans out through the CGP host) is also wired. `stella-serve` is
the exception: it builds its own binary and nothing in `stella-cli` links it,
so a change there never reaches a `stella` user.

---

## The `.stella/` directory (per-workspace state)

The CLI reads and writes a `.stella/` directory at the workspace root. An agent
editing Stella's own code should know what lives where:

| Path | Purpose |
|---|---|
| `.stella/memories/*.md` | Durable lessons baked into the byte-stable system prompt prefix. Sorted by filename, loaded once per session. Written by hand. |
| `.stella/skills/<slug>/SKILL.md` | Auto-promoted skills from recurring reflection lessons. Never enforced — selected and injected as volatile context. |
| `.stella/rules/*.toml` | Published **context records** — this repository's own steering policy, one record per file ([`docs/spec/adaptive-context/context-pr.md`](docs/spec/adaptive-context/context-pr.md)). The one part of `.stella/` that is **tracked in Git**, because a record only steers a teammate's session if it travels with the repository. Beside them, `governance.toml` sets the governance mode (this repo is `regulated`) and `promotions.jsonl` is the hash-chained ledger of enforcement grants and record lifecycle events (retirements and supersessions, #2728); `stella context validate` re-verifies both in CI on every PR. Edit through `stella context keep` / `promote`, not by hand. |
| `.stella/tools/*.toml` | Developer-defined custom script tools. Also scanned at `~/.stella/tools/`. |
| `.stella/settings.json` | Project-scope provider config (overrides built-ins or defines new providers) and tool switches (`tools.delegate: "off"` withholds the sub-agent delegation tool — every built-in is registered by default since #710). Merged per-field with org-managed and user scopes. |
| `.stella/mcp.toml` | MCP server config — extra tools merged into the registry at session start. An installed plugin may also ship one (`<plugin_dir>/mcp.toml`, declared as `[[mcp]]`, #4733); a server of yours keeps its name on a collision, and the package's copy is dropped with a notice. |
| `.stella/domains.toml` | Domain taxonomy for memory/reflection tagging, inferred by `stella init`. |
| `.stella/workspace.json` | Durable per-workspace telemetry identity (`workspace_id`), written by `stella cloud register`. Deliberately **outside** `private/` and safe to commit — sharing it makes every clone/machine report under one `workspace_id` to a cloud org. |
| `.stella/private/` | Owner-only generated local state (`0700`; files `0600`). The generated `.stella/.gitignore` excludes this whole directory. |
| `.stella/private/reflections.jsonl` | Per-turn reflection mining log (one JSON object per line). |
| `.stella/private/store.db` | Canonical local SQLite telemetry (executions, events, cost/tokens). Community/default has zero telemetry egress; an enrolled Enterprise seat may derive only the documented content-free operational rollup. Retention is opt-in via `stella stats prune` (`Store::prune`): dropping an execution explicitly cascades to the 13 tables keyed off `executions.id` — the schema declares no foreign keys — and never destroys telemetry the usage hub has not replicated yet without `--force`. |
| `.stella/private/context.db` | Recallable memories, episodes, facts, and temporal context. |
| `.stella/private/codegraph.db` | Tree-sitter code-graph index, built on `stella init`. |
| `.stella/private/fleet.db` | Fleet run, attempt, commit, and spend ledger — plus the `dispatch_claims` lease table, which is **not** the fleet's alone: `stella self-driving drive` claims `issue:<n>` there for as long as a turn is in flight, so two loops against one clone can see each other (#4300). A workspace that has never run `stella fleet` can therefore still have one. |
| `.stella/private/mcp_oauth.json` | MCP OAuth tokens. Secret local state; never commit it. |
| `.stella/private/mcp_auth_probes.json` | Connect-time 401 cache for MCP auth-probe suppression (#2687): server names + timestamps, 15-minute TTL, fails open. Not secret, but lives with the rest of the MCP auth state. |

Older releases wrote these private artifacts directly under `.stella/`. Path
resolvers migrate a safe, closed legacy file into `.stella/private/`; unsafe
permissions or live SQLite WAL/SHM sidecars fail closed with an actionable
error and leave the legacy files untouched.

Everything **user-global** lives under `~/.stella` on every platform (like
Claude Code's `~/.claude`) — no OS-specific data dir. `STELLA_HOME` moves the
whole home; the narrower `STELLA_DATA_DIR` still wins for the data tier where
it always did. Those two are the entire list of redirecting overrides
(`stella_home::OVERRIDE_ENV_VARS`), which is exact in both directions — a name
on it that resolves nothing declines the legacy-layout migration for a process
sitting on the defaults, which is what `STELLA_CONFIG_DIR` did until #2442
retired it. Key entries: `settings.json`, `credentials.toml`,
`skills/`, `agents/`, `rules/`, `tools/` (config); `usage.db` (the
cross-project telemetry hub), `sessions/`, `notifications/`, `catalog.db`,
`enterprise-telemetry.db`, `installation-id`, `cloud.json` (data). On first
run the CLI migrates the legacy split layout (platform data dir +
`~/.config/stella`) into `~/.stella`, per-entry and best-effort — an entry
that already exists at the new home is never overwritten.

| Global path | Purpose |
|---|---|
| `~/.stella/usage.db` | Cross-project **telemetry hub**: full-fidelity per-call rows replicated from every project's `store.db` via a durable per-project cursor, scoped by `org_id`/`workspace_id`/`repo_id`. Reads never touch project stores. |
| `~/.stella/cloud.json` | Stub cloud-account registration: `org_id` (+ a reserved `oauth_token` slot for the future login). `org_id`/`workspace_id` are NULL until `stella cloud register`. |

`stella usage report` reads the hub (per org/provider/model totals); `stella
usage sync [--all]` replicates above the cursor and heals projects whose
best-effort end-of-turn sync failed; `stella cloud status|register` manages
the identity that scopes it.

---

## Glossary — the identifiers that look alike

Six different ids in this workspace can all be read as "one thing the agent
did", and they are genuinely distinct entities owned by different crates. The
join keys are correct today (`crates/stella-observatory/src/db.rs` joins both
`execution_id` and `run_id`), so this is a naming hazard, not a bug — but read
this before assuming two of them mean the same thing:

| Term | Identifier | Owner | What it is |
|---|---|---|---|
| **session** | `SessionRecord::id` | `crates/stella-store/src/sessions.rs` | One run of the CLI, tracked in the cross-process registry under `~/.stella/sessions/`. Stamped onto `executions.session_id` (schema v8) so `Store::session_events` can reassemble a session's whole journal across its turns. |
| **execution** | `execution_id` | `crates/stella-store/src/ddl.rs` | One row in the `executions` table — the store's unit of work (one goal/turn) with its prompt, provider/model, outcome and cost. The foreign key every child telemetry table hangs off. |
| **dispatching execution** | `executions.parent_execution_id` | `crates/stella-store/src/dispatch.rs` | Which execution asked for another one (schema v36). A deck worker lane opens a real execution row of its own; this says whether a *turn* dispatched it or a person did, and NULL is the second answer, not a gap. A `delegate` child opens no row at all and is attributed by `sub_agent_id` on its events instead — the two mechanisms answer the same question about two different things. |
| **turn** | `turn_instance` | `crates/stella-protocol/src/event.rs` | One `run_turn` — a prompt through the model/tool loop to an answer. Monotonic per session; groups the steps of that turn in `step_manifest`/`step_receipt`. In the store one turn is one execution. |
| **step** | `(step, call_seq)` | `crates/stella-protocol/src/event.rs` | One iteration inside a turn: one model call plus the tools it requested. `call_seq` disambiguates the several calls that can share a `(turn_instance, step)` — the engine's worker call is 0, and the overflow summarizer and a plugin's declared seats take 1, 2, … Both `step_manifest` and `step_usage` carry the pair, so what a call saw and what it cost join on it; a `step_usage` line recorded before #4793 carries neither and is unjoinable, which it says by leaving both absent rather than defaulting `call_seq` to the worker's 0. |
| **fleet run** | `run_id` | `crates/stella-fleet/src/ledger.rs` | One multi-agent fan-out, top of the fleet hierarchy: run → task → attempt → commits/spend. **Not** an `execution_id` and **not** a session. |
| **task** | `stella_fleet::TaskId` / `stella_protocol::TaskId` | `crates/stella-fleet/src/plan.rs`, `crates/stella-protocol/src/task_id.rs` | One word, two entities, and since #5039 both of them have a type with the same name — read the crate. In the **fleet** ledger it is one unit of work dispatched to a worker within a run (a `String` alias). In the **board** it is a row of the agent's own task-board snapshot: the per-session ordinal `"1"`, `"2"`, …, mirrored from `TaskUpdate` events into the store's `tasks` rows keyed `(session, task id)`, and carried on the events that represent work (`events.task_id`) so a task has an evidence ledger and a per-task cost. `TaskItem::id` is still a `String` spelling of the second one (#5159). |

---

## Code style and conventions

- **`rustfmt` settles all formatting** — default config, no arguments. Don't
  hand-format. CI runs `cargo fmt --check`.
- **Clippy at `-D warnings`** across all targets. Do **not** `#[allow]` your way
  past a lint without a comment saying why the lint is wrong *here*.

  For `dead_code` and `unused_imports` this is **enforced**, by
  `scripts/check-dead-code-allows.py` (`make dead-code-allows`). Clippy cannot
  check it — silencing clippy is the attribute's whole job — so nothing in the
  gate could tell one of these from any other line, and the tree reached zero
  suppressions twice and drifted back both times (#3949).

  - **A floor.** Every suppression in production source carries an in-band
    `reason = "..."` or a comment line directly above it. Absolute, not
    ratcheted: unexplained at any count is a defect. A module doc (`//!`) does
    not count — it describes the file, not the item under it.
  - **A down-only ratchet** (`scripts/dead-code-allows-baseline.txt`), for the
    one reason a ratchet is ever legitimate here: the rule predates the guard,
    so the baseline records the debt #3872 left rather than granting new
    permission. `make dead-code-allows-update` refuses to raise a number or add
    a crate. A justified suppression still counts — a comment makes one
    *reviewable*, not *free*, and this tree accumulated twenty of them one
    reasonable-sounding paragraph at a time.

  **`#[cfg(test)]` is the better answer** whenever the only callers really are
  tests, and the guard is built to push you there: it does not count it.
  `#[allow(dead_code)]` asserts *the lint is wrong*; for an item nothing ships
  a call to, it is not. `#[cfg(test)]` asserts *this exists for the tests*,
  keeps it out of the binary, and is compiler-enforced — a later production
  caller becomes a build error instead of silently re-justifying the allow.
  Deletion is the other answer. Test paths, `#[cfg(test)]` bodies, and the
  platform-conditional `cfg_attr` form are excluded by construction rather than
  by allowlist. `make dead-code-allows-test` covers the guard's own directions.
- **A constant set by a measurement carries `/// MEASURED:` and a test.** The
  marker line says what was measured, when, and over how many samples; some
  test then names the constant, so a merge that reverts the value fails
  something. Enforced by `scripts/check-measured-constants.sh`
  (`make measured-constants`), which fails on a marked constant no test
  mentions, on a marker recording no measurement, and on one sitting above
  something that is not a `const` or a `static`. It has no baseline, because
  the marker is opt-in and a marked constant is one somebody marked in the same
  change; `make measured-constants-test` covers the guard's own directions.

  This is #2495's option 2, chosen over per-constant assertions as an unwritten
  policy for the reason the issue gives: an unwritten policy is opt-in and the
  next tuned constant forgets. The failure it exists for already shipped —
  #2414 moved a triage latency ceiling from 10s to 30s after measuring that
  27 of 34 triage calls burned the full 10s and returned nothing; #2462
  rewrote the surrounding struct literal from a branch that predated it and
  carried the 10s back over the top. Git reported no conflict, no test failed,
  and the field's own doc comment was left describing a number the struct no
  longer had. It was found weeks later, by someone starting the follow-up
  issue that depended on the 30s ceiling.

  What the marker is **not** for: a bound, a protocol value, or a number
  chosen by taste. Marking one of those makes the marker mean less everywhere
  it appears, and the guard cannot tell the difference — a reviewer can.
- **Name things for what they are, not what they were.** If you rename a
  concept, chase it through comments and docs in the same PR — stale comments
  are treated as bugs in review.
- **A module with submodules is `foo.rs` beside `foo/`, never `foo/mod.rs`.**
  `src/anthropic.rs` next to `src/anthropic/` is the layout Rust 2018
  introduced and the one this workspace uses: the parent declares its
  children and the children live in the folder named for it. It is what
  nearly every module here already does, and it is the form the Rust book
  presents as current.

  `mod.rs` is the pre-2018 spelling and is not used for library code. Its
  cost is what the book names: a tree where a dozen open editor tabs are all
  called `mod.rs` and only the directory tells them apart.

  **The one exception is an integration test's shared helper**, which must be
  `tests/common/mod.rs`. Cargo compiles every top-level file in `tests/` as
  its own test binary, so `tests/common.rs` would be built as a test crate of
  its own; putting it one level down as `mod.rs` is how cargo itself says to
  avoid that. The `tests/common/mod.rs` files in this tree are correct and
  stay.

  This is also how a file near the size limit gets its room: more hierarchy
  under `src/`, smaller files, same public names.
- **Doc comments on public items**, and on any function whose *why* isn't
  obvious from its body. No comments that narrate the next line.
- **No new dependencies casually.** Every new crate in `Cargo.toml` gets a
  sentence in the PR description justifying it.
- **Match the neighborhood.** Every crate has an established idiom — copy the
  patterns around you before inventing new ones. The module-level doc comment
  (`//!`) is the established entry point for each file; study a sibling before
  writing a new one.
- **Edition 2024, MSRV 1.90.** Workspace deps are centralized in the root
  `Cargo.toml` `[workspace.dependencies]` — reference them as
  `serde.workspace = true` in per-crate manifests.

### Commits

[Conventional Commits](https://www.conventionalcommits.org), with the crate or
surface as the scope, matching the existing history:

```text
feat(stella-model): add mistral provider adapter
fix(stella-tui): restore terminal on panic in raw mode
docs(readme): correct provider table
ci(release): sign macOS binaries
```

One logical change per PR. There is **no per-commit DCO sign-off** — this
project uses the CLA instead (see CONTRIBUTING.md's "Sign the CLA once"; the
CLA's license grant is what lets a contribution ship in commercial builds,
which a DCO sign-off cannot do). A `Signed-off-by` trailer is harmless but
carries no meaning here.

### Closing the issue on merge

Referencing an issue as `(#367)` in the **PR title** does not close it. GitHub
never parses the title for closing keywords — only the PR *description* and
*commit messages*. This repo accumulated a backlog of already-shipped issues
that stayed open for exactly that reason, so treat it as a hard rule:

> Put `Closes #N` in the PR description **and** as a trailer on a commit.

Both are required because the two merge paths read different text:

- **Squash** (the default here) composes the commit body from
  `COMMIT_MESSAGES`, *not* the PR body — so a `Closes #N` that exists only in
  the description never reaches the commit.
- **Rebase** (also enabled) replays your commits verbatim; the PR body is
  likewise never turned into a commit message.

The PR description's link closes the issue through GitHub's linked-issue
mechanism, and the commit trailer closes it through commit-message parsing on
the default branch. Belt and braces — either one alone is a silent single point
of failure, and the failure mode is invisible until someone audits the backlog.

```text
fix(stella-core): stop the step loop spinning on a wedged tool

The dispatch timeout never armed for tools that block before their first
poll, so a headless run could hang forever.

Closes #367
Signed-off-by: Ada Lovelace <ada@example.com>
```

Use `Closes` for bugs and completed features, `Refs #N` when a PR advances an
issue without finishing it — `Refs` deliberately does not close. One issue may
be closed by exactly one PR; if a fix spans several, close on the last and
`Refs` the rest.

**One keyword per issue.** `Closes #A, #B` closes `#A` and leaves `#B` open:
GitHub parses the keyword and the reference immediately after it, and reads the
rest of the list as prose. Write `Closes #A, Closes #B`. This is the same
failure the section opens with — an issue whose work shipped, staying open, with
nothing to notice it but an audit — reached by a different wrong guess about
what the parser reads, and #5210 is where it happened.

---

## Testing approach

- **Property tests** for pure engine logic (`proptest`): loop detection,
  retry history, skill selection, and the task board (`stella-core`), plus
  retrieval fusion (`stella-context`), fleet planning (`stella-fleet`), and
  render/scroll (`stella-tui`). These run on every `cargo test`. Compaction,
  eviction, and budget arithmetic are covered by unit tests, not properties —
  a property test for them is a welcome contribution. Witness-verification
  property tests (`flip_requires_a_prior_failing_observation` and its
  siblings) lived here through `stella-pipeline`; that crate is deleted
  (#3865), and the property carries over to whatever verification plugin
  ports it (`doc:pipeline-as-plugins` §8) rather than living in this
  workspace.
- **Witness tests** for features — see above.
- **Wiremock-based adapter tests** for provider SSE parsing and HTTP error
  classification (`stella-model`, `stella-mcp`).
- **Integration tests** with fixture MCP servers (`crates/stella-mcp/tests/`).
- **Golden frames** for the command deck
  (`crates/stella-tui/tests/deck_render_snapshots.rs`). Each tab and overlay renders
  into a fixed-size `TestBackend` and the whole character grid is compared
  against a committed snapshot under `tests/snapshots/deck/`. This catches what
  a `contains` assertion cannot — a column that shifted, a panel that moved, a
  row that vanished. Regenerate with
  `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`, then **read
  the diff**: a golden blessed without looking is a changelog, not a test.

When iterating, run a single crate's tests — `cargo test -p stella-core` is
seconds; `cargo test --workspace` rebuilds everything.

---

## Gotchas

- **`Cargo.lock` is tracked.** Stella ships a binary and `install.sh` builds
  with `--locked`, so the lockfile must be committed and reproducible. Nothing
  you run day to day passes `--locked`, which is what makes a stale lock
  invisible until release time — so `lockfile-sync`
  (`scripts/check-lockfile-sync.sh`) resolves it on every gate run, including
  the `guards-fast` rung the pre-push hook picks. It compiles nothing.

  It catches the lock you forgot to regenerate. It cannot catch the other
  shape: two branches that are each correct and collide only once both land —
  the lockfile is one shared cell every version-bumping PR writes, exactly like
  `scripts/file-size-baseline.txt` above. That happened twice on 2026-08-16
  (#3311, then the 0.9.50 sync against it) and left `main` red for every
  `--locked` build. Nothing pre-merge can see it, because neither author's tree
  is wrong — which is why `main-canary.yml` re-asks the same question after the
  merge and files an issue when the answer changed (#3332). The two halves are
  deliberately separate: one stops you shipping a stale lock, the other bounds
  how long `main` stays broken when nobody did.
- **A grep over cargo's output is not a build result; the exit code is.**
  Cargo colourises when it thinks it is talking to a terminal, which puts the
  escape *before* the word — a line that reads
  `error[E0624]: method is private` on screen is
  `\x1b[1m\x1b[91merror[E0624]\x1b[0m…`, so `rg '^error'` matches nothing and
  prints a reassuring blank. That is not "no errors"; it is a filter that
  cannot see them. In PR #3005 a `cargo build --workspace` was declared clean
  on exactly that filter while `stella-cli` was failing to compile with six
  `documentation comments cannot be applied to function parameters` errors, and
  the mistake survived two more commands before an exit code contradicted it.
  A pipe compounds it: `$?` becomes the filter's status, so `cargo build … | rg
  …` reports on `rg`. Redirect to a file and read `$?`; pass `--color=never`
  (or `CARGO_TERM_COLOR=never`) when you filter deliberately; anchor on
  `error\[` rather than `^error` if you filter coloured output anyway.
- **`.cargo/config.toml` is gitignored** — it holds per-developer cargo aliases
  (`tc` = test stella-core, etc.). It's not committed.
- **Settings 3-scope merge**: user → org-managed (`STELLA_MANAGED_SETTINGS`) →
  project (`.stella/settings.json`). Project wins per-field.
- **`context.db` vs `codegraph.db`**: `stella-context` and `stella-graph` used
  to share `.stella/private/context.db` — they now use separate files
  (`.stella/private/context.db` and `.stella/private/codegraph.db`
  respectively). Don't revert this.

## Demo recordings — what a "demo" of Stella means

A demo video shows **Stella itself working**: the TUI running a real agent
task — planning turns, fanning out tools, editing files, passing tests on
something it is building. It is sales/marketing material.

- Footage of *this repo compiling* (`cargo build`, `cargo test`, clippy) is
  **never** demo content. `scripts/demo-scenario.sh` (the build marathon) is a
  recorder stress-test for `scripts/record-demo.sh`, nothing more.
- Stella is BYOK: a real demo needs a provider API key. If none is configured,
  **stop and ask for one** before recording — do not substitute footage of
  something else that happens to run without credentials.
- Before recording anything intended for an audience (demo, docs, social),
  confirm what the video must *show*, not just what command to run.

---

## Standing decisions — apply without asking

Each directive below is a Steering Context Record in [`docs/scr/`](docs/scr/);
the SCR is canonical — it carries the rationale, exceptions, and enforcement
status. This block is the compiled summary that every agent — Claude Code
(via CLAUDE.md's `@AGENTS.md` import) and Stella (which reads AGENTS.md
directly) — loads at session start. The corpus is identical across the
macanderson org repos.

- **[SCR-001](docs/scr/SCR-001-no-full-suite-builds.md) — Tests/builds
  (inner loop):** Never compile or run the full test suite while developing.
  Build and test only the crates/packages/modules touched by the change
  (plus direct dependents on interface changes). The full suite is CI's job.
  Here: `cargo test -p <crate> [filter]`, never bare `cargo test` / `cargo test --workspace`.
- **[SCR-002](docs/scr/SCR-002-durability-first-architecture.md) —
  Architecture decisions:** Do not ask. Choose the most durable option — the
  one that can't be questioned in 10 years as the right move. Cheap-and-easy
  only wins when it is also the excellent durable choice. Record every such
  decision as an ADR in `docs/adr/`; the ADR replaces the question.
- **[SCR-003](docs/scr/SCR-003-dod-verified-close.md) — Definition of
  done:** An issue closes only when every DoD checklist item is satisfied
  and verified. Reference-grade includes tests, code comments, docs, and
  CI — not just the implementation.
- **[SCR-004](docs/scr/SCR-004-residue-becomes-issues.md) — Residue:**
  Before declaring any task complete, file a GitHub issue for every
  follow-up, tech-debt item, or logical next step you noticed. Apply ONLY
  the `triage` label.
- **[SCR-005](docs/scr/SCR-005-triage-separation-of-duties.md) — Triage
  separation of duties:** Never apply a priority or size label —
  a dedicated triage agent owns sizing and priority; a guard workflow
  strips creator-applied priorities.
