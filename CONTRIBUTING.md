# Contributing to Stella

```text
   ·  .  ✦   ·        ·   ✦        .   ·      ✦   .        ·
   verified done, not claimed done — and that includes your PR.
```

Thanks for wanting to make Stella better. This document is the whole game:
how to set up, where your change goes, what "done" means here, and how to
get it merged. It's long because it's honest — but the short version is:

> **Ship a witness test, keep the gates green, sign your commits. That's it.**

- [Ways to contribute](#ways-to-contribute)
- [Development setup](#development-setup)
- [Where does my change go? — a workspace tour](#where-does-my-change-go--a-workspace-tour)
- [The ground rules](#the-ground-rules)
- [The definition of done — witness tests](#the-definition-of-done--witness-tests)
- [Style](#style)
- [Commits, the CLA, and PRs](#commits-the-cla-and-prs)
- [Issues and labels](#issues-and-labels)
- [Security](#security)
- [License](#license)

## Ways to contribute

Every one of these is genuinely valued — pick the one that fits your energy:

| Contribution | Where to start | Effort |
|---|---|---|
| 🐛 **A bug report with a repro** | [Bug report form](https://github.com/macanderson/stella/issues/new?template=bug_report.yml) | 10 minutes |
| 🧭 **Docs & examples** — fix a lie in the docs before it fools someone else | `website/content/docs/**.mdx` for anything a *user* reads, `docs/**` for contributor-facing specs, plus `README.md`, `--help` text, doc comments | Small |
| 🔌 **A new provider adapter** — Stella is BYOK; every model provider we speak makes it more useful | `stella-model/src/` — copy the shape of an existing adapter | Medium |
| 🛠 **A new built-in tool** | `stella-tools/src/` — implement the tool trait, register it in `ToolRegistry`, then declare one line in [`catalog.rs`](stella-tools/src/catalog.rs) | Medium |
| 🌐 **A Context Graph Protocol (CGP) provider** — implement it in your language and prove it green | [macanderson/context-graph-protocol](https://github.com/macanderson/context-graph-protocol) — its own repo, no Stella code required | Medium |
| 🏗 **Core engine work** | `good first issue` / `help wanted` labels | Varies |

If you're not sure where something fits, open an issue first — a ten-line
sketch of the idea saves a thousand-line PR that can't merge.

## Development setup

**Prerequisites:** Rust **1.90+** via [rustup](https://rustup.rs) (the toolchain
is pinned in `rust-toolchain.toml`, so rustup will fetch the right one
automatically), `git`, and optionally [`ripgrep`](https://github.com/BurntSushi/ripgrep)
and [`fd`](https://github.com/sharkdp/fd) (the agent's `grep`/`glob` tools shell
out to them at runtime).

```bash
git clone https://github.com/macanderson/stella.git
cd stella

cargo build --workspace          # first build compiles bundled SQLite — quick
cargo test  --workspace          # the full suite
cargo run -p stella-cli -- models   # smoke-check your build
```

Iterating on a single crate is much faster than the whole workspace:

```bash
cargo test  -p stella-core       # just the engine
cargo clippy -p stella-tools --all-targets -- -D warnings
```

### The gate — run before every push

A red gate is an automatic "not yet":

```bash
./scripts/check-no-scratch.sh
./scripts/check-action-pins.sh
./scripts/check-invariants.sh
python3 ./scripts/check-doc-links.py check
./scripts/check-file-size.sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Or just `make gate`, which is the nine of them in order.

CI enforces the same steps, split across `ci.yml` (plus a release smoke build)
and `docs-guards.yml`, which runs the two prose guards on their own because they
trigger on the `docs/**` and `*.md` paths that `ci.yml` deliberately ignores.

**Cite a document by its id, not its path.** `doc:context-reuse §4` resolves no
matter where the file moves; a document with no frontmatter `id` is not citable
at all (`make doc-adopt DOC=…` gives it one). Legacy path citations repair
themselves — `make doc-links-fix` repoints them after a move. Cite anything
outside this repository by URL. See `docs/README.md § How to cite a document`.

**Run `make hooks` once per clone.** It points `core.hooksPath` at `.githooks`,
whose `pre-push` hook runs `make gate` and aborts the push if it fails — so a
red gate costs you thirty seconds locally instead of a review round-trip. It is
advisory and per-clone (`SKIP_GATE=1 git push` bypasses it), not a substitute
for the server-side checks.

The hook scopes the three compile tiers — clippy, rustdoc, test — to the crates
your diff can actually reach, so a change confined to one crate no longer pays
for all 21 members (#1135). It falls back to the whole workspace for a push to
`main`, a tag, a diff touching a workspace-root manifest / `Cargo.lock` / a
build script / the gate machinery, and for anything it cannot narrow with
confidence. See what it would choose with `make impacted`. Under time pressure,
step down a rung rather than switching the gate off:

```bash
GATE=fast git push       # guards + fmt + clippy — no rustdoc, no tests
GATE=full git push       # the whole workspace, whatever the diff says
SKIP_GATE=1 git push     # nothing at all (emergencies)
```

`check-no-scratch.sh` asserts that no tracked file matches a `.gitignore` rule —
agent-session scratch (repro trees, plans, memory files) must never reach the
remote (#448). If it fails, either untrack the path with
`git rm -r --cached <path>` (the files stay on your disk) or narrow the ignore
rule that is catching real content.

### Changing the docs

The user-facing documentation is not in this repo's markdown — it is the MDX
under `website/content/docs/`, published at
[stella.oxagen.sh](https://stella.oxagen.sh). It needs Node ≥ 20 and pnpm, and
nothing else; you never have to build Rust to fix a docs page:

```bash
cd website                   # the site owns its own manifest and lockfile
pnpm install                 # once
pnpm dev                     # the site at http://localhost:3400
pnpm typecheck && pnpm build # what the docs CI job runs
```

Two rules the site enforces silently, so check both before you push:

- Every page needs `title` and `description` frontmatter.
- A new page must be added to the nearest `meta.json` `pages` array. That array
  is an allowlist for the sidebar, not a build input — an unlisted page still
  builds and still answers at its URL, it is just invisible.

A docs-only PR runs the fast `docs` workflow instead of the Rust gate, and
needs no witness test. Contributor-facing material — design specs, ADRs, the
research papers — stays in [`docs/`](docs/README.md) instead; several of those
specs are cited by `file:section` from Rust doc comments, so renaming one means
chasing the citations in the same PR.

## Where does my change go? — a workspace tour

Nineteen crates sounds like a lot; the rule of thumb is one sentence each:

| You want to… | Go to |
|---|---|
| Change how the agent loop plans / retries / compacts / budgets | `stella-core` (**no I/O allowed here** — see ground rules) |
| Add or fix a model provider (SSE, tool-call dialect, pricing) | `stella-model` |
| Add or fix a built-in tool (`read_file`, `bash`, `verify_done`, …) | `stella-tools` |
| Change a CLI command, flag, or the agent wiring | `stella-cli` |
| Change the REPL rendering / panels / keybindings | `stella-tui` |
| Touch shared types crossing a crate boundary | `stella-protocol` (zero logic, zero I/O — types only) |
| Log why the program did something (not what the agent did) | `stella-diag` (a leaf; a field that could carry a path or model output will not compile) |
| Persistence: executions, events, telemetry (SQLite) | `stella-store` |
| Retrieval: graph, embeddings, episodic memory | `stella-context` |
| Tree-sitter code indexing | `stella-graph` |
| The triage → … → judge orchestration plane | `stella-pipeline` |
| MCP client (external tool servers) | `stella-mcp` |
| Multimodal generation | `stella-media` |
| Multi-agent fan-out, worktree isolation | `stella-fleet` |
| The Observatory telemetry dashboard (`stella observe`) | `stella-observatory` |
| The headless engine server a host process drives over the wire | `stella-serve` (its own binary, not linked into the CLI) |
| The Context Graph Protocol (wire types / host / conformance) | external repo: [`context-graph-protocol`](https://github.com/macanderson/context-graph-protocol) |

Every crate except `stella-serve` ships in the CLI today: `stella-pipeline` drives the default
`stella run` path, `stella-fleet` powers `stella fleet`, `stella-tui` is the
Command Deck (the default interactive shell on a TTY), and `stella-media`
provides image generation via the `generate_image` tool. The context/graph
plane is wired too — `stella init` builds the code-graph index and recall fans
out through the CGP host. For what each crate is, see the crate table in the
[README](README.md#workspace-layout); for what actually reaches a `stella`
user, see **Status — what ships** in
[AGENTS.md](AGENTS.md#workspace-layout--where-a-change-goes).

## The ground rules

These are the architectural invariants the whole design hangs on. PRs that
break them will be asked to restructure, no matter how good the feature is.

**They are stated once, normatively, in
[AGENTS.md § Architecture: ports, not concretions](AGENTS.md#architecture-ports-not-concretions)
— read them there before your first PR.**

That list is the single source, and its numbering is part of the contract:
Rust doc comments and crate READMEs cite invariants by number (`content_free.rs`
cites "AGENTS.md invariant #3", `stella-model/README.md` cites #8), so the
numbers are addresses, not decoration. This file used to carry a second, silently
abridged copy — seven of the eight, with #8 missing entirely and #3 shorn of the
half that says how it is enforced. Two copies of a normative rule is not
redundancy; it is a coin flip over which one a reader obeys, and nothing tells
them they got the short one.

## The definition of done — witness tests

Stella refuses to call a task done until a test **fails on the old code and
passes on the new** — and we hold contributions to the same contract, because
a merely-green suite can hide unwired features and vacuous tests.

For a behavior change or feature, your PR should include a **witness test**:

- it **fails** on `main` without your change (the feature is genuinely absent),
- it **passes** with your change (the feature is genuinely present).

You can check this the artisanal way (`git stash && cargo test -p <crate>`),
or let Stella verify Stella — build it and run your task through the
`verify_done` gate, which automates exactly this in a shadow worktree.

Pure refactors, docs, and CI changes don't need a witness — say so in the PR
template and move on. If a witness is genuinely impractical (e.g. TUI
rendering), explain how you verified the change instead.

## Style

- **`rustfmt` settles all formatting arguments** — default config, no debates.
- **Clippy at `-D warnings`** across all targets. Don't `#[allow]` your way
  past a lint without a comment saying why the lint is wrong here.
- **Name things for what they are**, not what they were. If you rename a
  concept, chase it through comments and docs in the same PR — stale comments
  are treated as bugs in review.
- **Doc comments on public items**, and on any function whose *why* isn't
  obvious from its body. No comments that narrate the next line.
- **No new dependencies casually.** Every new crate in `Cargo.toml` gets a
  sentence in the PR description justifying it. `contextgraph-types` stays
  dependency-light as a matter of contract.
- **Match the neighborhood.** Every crate has an established idiom — copy the
  patterns around you before inventing new ones.

## Commits, the CLA, and PRs

**Commit format** — [Conventional Commits](https://www.conventionalcommits.org),
with the crate (or surface) as scope, matching the existing history:

```text
feat(stella-model): add mistral provider adapter
fix(stella-tui): restore terminal on panic in raw mode
docs(readme): correct provider table
ci(release): sign macOS binaries
```

**Close the issue you fixed.** Put `Closes #N` in the PR description *and* as a
trailer on a commit. A `(#367)` in the PR **title** closes nothing — GitHub only
reads closing keywords from the description and from commit messages. Both spots
are needed because squash merges build the commit body from commit messages
rather than the PR body, while the description drives GitHub's linked-issue
close. Use `Refs #N` when a PR advances an issue without finishing it.

**Sign the CLA once.** On your first PR a bot will ask you to sign the
[Contributor License Agreement](CLA.md); it takes one comment and covers every
PR you open afterwards. **You keep your copyright** — the CLA is a license
grant, not an assignment. See [License](#license) below for what it grants and
why. There is no per-commit sign-off step.

**PR checklist** (the template walks you through it):

1. One logical change per PR — smaller lands faster.
2. The gate is green locally (`fmt` / `clippy -D warnings` / `test`).
3. A witness test, or a stated reason there isn't one.
4. Docs updated in the same PR if behavior or flags changed (`README.md`,
   `--help` text, doc comments).
5. CLA signed — the bot prompts you on your first PR.

Maintainers aim for a first response within a few days. "Needs work" is a
normal part of the loop here, not a rejection.

## Issues and labels

- **[Bug report](https://github.com/macanderson/stella/issues/new?template=bug_report.yml)** — include `stella --version`, OS, provider/model, and a repro.
- **[Feature request](https://github.com/macanderson/stella/issues/new?template=feature_request.yml)** — say what you're trying to do, not just what to add.

Labels you'll see: `area:*` routes an issue to a crate; `P0`–`P2` is priority;
`good first issue` and `help wanted` mean what they say; `needs-witness` means
a PR is waiting on its witness test.

## Security

Found a vulnerability? **Don't open a public issue.** See
[`SECURITY.md`](SECURITY.md) — we use GitHub's private vulnerability
reporting.

## License

Stella is dual-licensed: **[AGPL-3.0-only](LICENSE)** for everyone, plus
commercial licenses for users who cannot accept the AGPL's reciprocal terms.
[`LICENSING.md`](LICENSING.md) explains both tracks.

By contributing you agree to the [Contributor License Agreement](CLA.md). In
short: **you keep your copyright**, your contribution is published under the
AGPL, and Oxagen may also include it in commercially licensed builds. Without
that last part a merged contribution would be AGPL-only and could never ship
commercially — which is why this project uses a CLA rather than a DCO sign-off.

If some clause in the CLA is a blocker for you, say so in the PR or email
<licensing@oxagen.sh>. Better to talk about it than to lose the contribution.

```text
   ·  .  ✦   ·        see you in the diff.        ·   ✦  .  ·
```
