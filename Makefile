# Stella — developer convenience targets.
# Run `make help` for the full list.

BUMP ?= patch

# `make record-demo` knobs — see scripts/record-demo.sh for the full set.
LIMIT ?= 0
TARGET ?= 60

# Where `make record-demo-video` parks the intermediate frame film. Under /tmp by
# default because it is a build artifact, not a source: it re-renders from the
# deck in a couple of minutes and is regenerated whenever the deck moves. Point
# FILM at a real path to keep one for inspection or a re-render.
FILM ?= $(shell mktemp -t stella-deck-film.XXXXXX.jsonl)

# Which cargo packages the three *compile* tiers of the gate cover — clippy,
# rustdoc and test (#1135). The default is the whole workspace, so `make gate`
# and CI behave exactly as they always have; nothing narrows unless a caller
# asks it to:
#
#   make gate                                  every member (CI, `main`, releases)
#   make gate CARGO_SCOPE="-p stella-cli"      one member and its dependents
#
# `.githooks/pre-push` fills this in from the pushed diff via
# scripts/impacted-crates.sh, which answers `--workspace` for anything it
# cannot narrow with confidence. The cheap global guards below are deliberately
# NOT scoped — a 1500-line file or an unpinned action is a fact about the
# repository, not about a crate.
CARGO_SCOPE ?= --workspace

# The guard tiers, named so the gate, the fast check and the hook can compose
# them instead of restating the list three times and drifting apart.
# GATE_GUARDS_FAST needs no toolchain at all (shell scripts over the tree);
# wire-schema joins in GATE_GUARDS because it runs the two schema exporters,
# which is a cargo build. Every rung below runs the full GATE_GUARDS — `check`
# compiles the workspace for clippy anyway, so excluding wire-schema there
# saved nothing and let a GATE=fast push land stale generated wire artifacts.
GATE_GUARDS_FAST := no-scratch no-secrets design-refs action-pins cargo-install-pins \
                    license-allowlist-parity repro-wiring shellcheck invariants doc-links \
                    command-docs brand-case file-size god-files gate-parity left-behind \
                    role-names stat-portability module-reachability typed-errors \
                    diagnostic-codes bench-suites
GATE_GUARDS := $(GATE_GUARDS_FAST) wire-schema

# The cargo steps that resolve or parse but never build. They are not in
# GATE_GUARDS_FAST because that variable's contract is literally "a shell script
# over the tree" and these two shell out to cargo — but they cost about a second
# between them and compile nothing, so every rung from guards-fast up can afford
# them. Named once here rather than restated on each tier line, which is the
# drift this file's tiering exists to prevent (#3332).
GATE_NO_BUILD := lockfile-sync format-check

# The whole gate, in order, as one name. `gate` below is defined *from* this
# variable rather than restating it, and `make print-gate-steps` prints it —
# which is what lets scripts/check-gate-parity.sh hold AGENTS.md and
# CONTRIBUTING.md to the real list instead of a hand-copied one. Both documents
# had already re-rotted twice, in the same direction each time: a guard was
# added here and the prose kept the old count (#1437).
GATE_STEPS := $(GATE_GUARDS) $(GATE_NO_BUILD) doc-warnings lint test tool-docs \
               self-driving-test

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*##' Makefile | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' | \
		sort

.PHONY: build
build: ## Build the full workspace (debug)
	cargo build --workspace

.PHONY: build-release
build-release: ## Build the shipping binary (release, optimized)
	cargo build --release -p stella-cli

# Deliberately separate from `build-release`: this is the *release* build, with
# $CARGO_HOME and the rustup sysroot remapped out of the binary so its SHA-256
# is a property of the source and not of your home directory (#910). It costs a
# full rebuild the first time because RUSTFLAGS is part of cargo's fingerprint,
# which is exactly why the everyday target above does not set them.
.PHONY: repro-build
repro-build: ## Build the shipping binary reproducibly for TRIPLE=<target> and print its SHA-256
	@test -n "$(TRIPLE)" || { echo "usage: make repro-build TRIPLE=x86_64-unknown-linux-gnu"; exit 2; }
	@./scripts/repro-build.sh $(TRIPLE)

.PHONY: smoke
smoke: ## Compile check — runs `stella models` (no API key needed)
	cargo run -p stella-cli -- models

.PHONY: format
format: ## Format all code (rustfmt)
	cargo fmt

.PHONY: format-check
format-check: ## Check formatting without modifying (CI gate)
	cargo fmt --check

.PHONY: lint
lint: ## Run clippy with -D warnings (CI gate; CARGO_SCOPE to narrow)
	cargo clippy $(CARGO_SCOPE) --all-targets -- -D warnings

.PHONY: fix
fix: ## Auto-fix clippy lints + format
	cargo clippy --fix --allow-dirty --workspace --all-targets -- -D warnings
	cargo fmt

.PHONY: test
test: ## Run the test suite (all crates; CARGO_SCOPE to narrow)
	cargo test $(CARGO_SCOPE)

.PHONY: test-core
test-core: ## Test stella-core only (fast engine iteration)
	cargo test -p stella-core

.PHONY: test-model
test-model: ## Test stella-model only (provider adapters)
	cargo test -p stella-model

.PHONY: test-tools
test-tools: ## Test stella-tools only (built-in tools)
	cargo test -p stella-tools

.PHONY: test-cli
test-cli: ## Test stella-cli only (the shipping binary)
	cargo test -p stella-cli

.PHONY: test-protocol
test-protocol: ## Test stella-protocol only (shared types)
	cargo test -p stella-protocol

# The trace-replay learning harness (#2304, `doc:trace-replay-learning-harness`
# §8). Deliberately NOT a `make gate` step: the assertions are ordinary in-crate
# tests already covered by the gate's existing `test` step, and a target whose
# job is to PRINT a report adds nothing as pass/fail — while every added step is
# one more shared cell the Makefile, AGENTS.md and CONTRIBUTING.md must agree on.
.PHONY: replay-learning
replay-learning: ## Replay the synthetic months corpus and print what the learners built (#2304)
	cargo test -p stella-cli --bin stella \
	  memory::replay::tests::replay_learning_report -- --exact --nocapture

.PHONY: record-demo
record-demo: ## Record a terminal timelapse (LIMIT=mins TARGET=secs CMD="..."; defaults to the multi-hour marathon)
	./scripts/record-demo.sh --limit $(LIMIT) --target $(TARGET) $(if $(CMD),-- bash -c '$(CMD)',)

# The command deck as a 1080p60 hero video. Two stages on purpose: the film is
# a deterministic byproduct of `render_deck` (so it re-renders byte-identically
# and diffs like source), and the renderer turns it into pixels. `--release` is
# not optional — a debug build spends about twenty minutes folding the session
# once per frame.
#
# Re-cut this on every release. The film is a picture of the deck, so it goes
# stale exactly when the deck changes — a tab added, a panel moved, a status
# field renamed — and a demo that shows a UI the download no longer has is the
# kind of claim this repository does not make. It costs about fifteen minutes
# and needs no API key. RELEASING.md carries the step.
.PHONY: record-demo-video
record-demo-video: ## Re-cut docs/demo/stella-deck.mp4 from the command deck (FILM=path to keep the intermediate)
	cargo run -q --release -p stella-tui --example deck_film > $(FILM)
	./scripts/render-deck-film.py $(FILM) -o docs/demo/stella-deck.mp4 \
		--poster docs/demo/stella-deck-poster.png --poster-frame 1100
	@echo "Re-cut. Watch it before committing — the shot list frames rows the"
	@echo "deck's own layout decides, so a layout change can move content out"
	@echo "of shot without failing anything."

# Every Python suite `.github/workflows/bench.yml` runs, and no other. The list
# is NOT here: scripts/bench-suites.sh reads it out of that workflow, because a
# second hand-written copy is what produced #2847 — the workflow ran seven
# suites, this target ran three, and the arenabench failure that reddened `main`
# on 2026-08-11 had no local command that would have caught it.
#
# Deliberately NOT a `make gate` step. It takes minutes (arenabench alone is
# ~70s) and would tax every Rust-only push for a question that push cannot have
# changed. .githooks/pre-push runs it instead for a push whose diff matches the
# workflow's own scope filter, so the suites are gated where they are relevant
# and free where they are not. `make bench-suites` — which IS a gate step —
# holds the arrangement together without running a single test.
.PHONY: bench-test
bench-test: ## Run every Python bench suite .github/workflows/bench.yml runs (#2847)
	@./scripts/bench-suites.sh run

# Deliberately NOT a `make gate` step: it reads S3 and talks to GitHub, and the
# gate must stay runnable offline and side-effect-free. `gate-parity` would fail
# the moment this joined GATE_STEPS, which is the guard working.
#
# The default is a dry run — it prints the exact issue bodies and comments it
# would post and writes nothing. `ARGS=--apply` is the only thing that writes.
.PHONY: triage-bench-traces
triage-bench-traces: ## Triage a bench run's traces into issue activity (RUN=w10p MIRROR=path|FETCH=1; dry run unless ARGS=--apply)
	@[ -n "$(RUN)" ] || { echo "usage: make triage-bench-traces RUN=<run-id> [MIRROR=path | FETCH=1] [ARGS=--apply]"; exit 2; }
	python3 bench/trace_triage/triage_bench_traces.py --run $(RUN) \
		$(if $(MIRROR),--mirror $(MIRROR),) $(if $(FETCH),--fetch,) $(ARGS)

# Reads a fetched run directory and writes a report beside the match assembled
# from it. Offline and read-only apart from those two files, but deliberately
# not a `make gate` step either: it needs a run to read, and the gate must be
# runnable on a fresh checkout.
.PHONY: postmortem
postmortem: ## Probe a fetched run's traces into a report on its match (RUN=path/to/run-dir)
	@[ -n "$(RUN)" ] || { echo "usage: make postmortem RUN=arenabench-cloud/<run-id>"; exit 2; }
	python3 bench/trace_triage/postmortem.py $(RUN) $(ARGS)

.PHONY: no-scratch
no-scratch: ## Assert no tracked file is gitignored (agent scratch guard, #448)
	@./scripts/check-no-scratch.sh

.PHONY: no-secrets
no-secrets: ## Assert no private key material is tracked
	@./scripts/check-no-secrets.sh

.PHONY: design-refs
design-refs: ## Assert nothing outside docs/design cites it (docs/design is work-in-flight)
	@./scripts/check-design-refs.sh

.PHONY: action-pins
action-pins: ## Assert every workflow `uses:` is pinned to a commit SHA (#648)
	@./scripts/check-action-pins.sh

.PHONY: cargo-install-pins
cargo-install-pins: ## Assert every workflow `cargo install` names an exact version (#915)
	@./scripts/check-cargo-install-pins.sh

.PHONY: license-allowlist-parity
license-allowlist-parity: ## Assert deny.toml and dependency-review.yml agree on allowed licenses (#920)
	@./scripts/check-license-allowlist-parity.sh

.PHONY: repro-wiring
repro-wiring: ## Assert both release paths build through scripts/repro-build.sh (#910)
	@./scripts/check-repro-wiring.sh

.PHONY: invariants
invariants: ## Assert the architectural invariants have one home and stable numbering (#630)
	@./scripts/check-invariants.sh

.PHONY: doc-links
doc-links: ## Assert every doc citation resolves to an identified document
	@python3 ./scripts/check-doc-links.py check

.PHONY: doc-links-fix
doc-links-fix: ## Repoint citations after a doc moved, and rewrite docs/manifest.json
	@python3 ./scripts/check-doc-links.py fix

.PHONY: doc-links-fix-by-name
doc-links-fix-by-name: ## ...also repoint broken paths by filename (a guess — read the list first)
	@python3 ./scripts/check-doc-links.py fix --by-name

.PHONY: doc-report
doc-report: ## Which documents are stale, superseded, or cited by nothing
	@python3 ./scripts/check-doc-links.py report

.PHONY: doc-adopt
doc-adopt: ## Scaffold frontmatter onto a document so it can be cited: make doc-adopt DOC=path
	@test -n "$(DOC)" || { echo "usage: make doc-adopt DOC=docs/design/thing.md"; exit 2; }
	@python3 ./scripts/check-doc-links.py init $(DOC)

.PHONY: command-docs
command-docs: ## Assert every stella subcommand has a listed reference page (#993)
	@./scripts/check-command-docs.sh

.PHONY: brand-case
brand-case: ## Assert docs prose spells the wordmark lowercase (#1500)
	@./scripts/check-brand-case.sh

# The generated per-tool reference. Deliberately NOT scoped by CARGO_SCOPE:
# the artifact is derived from stella-tools' catalog and stella-cli's session
# layers at once, so a push narrowed to either crate must still re-derive the
# whole directory. It needs no network and no model — every input is either
# source or the committed example fixture.
.PHONY: tool-docs
tool-docs: ## Assert docs/tools/ still describes the declared tools
	@./scripts/check-tool-docs.sh

.PHONY: tool-docs-update
tool-docs-update: ## Regenerate docs/tools/ after a tool change (commit the diff!)
	@STELLA_REFRESH_TOOL_DOCS=1 cargo test --quiet -p stella-cli --bin stella tool_docs
	@git --no-pager diff --stat -- docs/tools || true

.PHONY: wire-schema
wire-schema: ## Assert docs/wire/ still describes the AgentEvent wire format (#971)
	@./scripts/check-wire-schema.sh

.PHONY: wire-schema-update
wire-schema-update: ## Regenerate docs/wire/ after an AgentEvent change (commit the diff!)
	@./scripts/export-agentevent-schema.sh

.PHONY: file-size
file-size: ## Assert no new Rust or Python file exceeds the 1500-line ratchet (#629, #825)
	@./scripts/check-file-size.sh

.PHONY: file-size-update
file-size-update: ## Retighten the 1500-line ratchet baseline (run after splitting a file)
	@./scripts/check-file-size.sh --update

.PHONY: typed-errors
typed-errors: ## Assert no library crate's public API returns Result<_, String> (invariant #5)
	@python3 ./scripts/check-typed-errors.py

.PHONY: typed-errors-update
typed-errors-update: ## Retighten the invariant-#5 ratchet (run after typing signatures)
	@python3 ./scripts/check-typed-errors.py --update

.PHONY: typed-errors-test
typed-errors-test: ## Test the invariant-#5 ratchet's direction (hermetic; not part of `gate`)
	./scripts/test-typed-errors.sh

.PHONY: shellcheck-guard-test
shellcheck-guard-test: ## Test the shellcheck step's presence guard (hermetic; not part of `gate`)
	./scripts/test-shellcheck-guard.sh

.PHONY: diagnostic-codes
diagnostic-codes: ## Assert docs/reference/diagnostics.md documents every emitted diagnostic code (#2507)
	@./scripts/check-diagnostic-codes.sh

.PHONY: diag-reference
diag-reference: ## Regenerate the diagnostic-code reference from the tree, preserving prose (#2507)
	@python3 ./scripts/diagnostic-codes.py write

.PHONY: doc-warnings
doc-warnings: ## Assert rustdoc is clean workspace-wide, private items included (#634, #2336; CARGO_SCOPE to narrow)
	RUSTDOCFLAGS="-D warnings" cargo doc $(CARGO_SCOPE) --no-deps --document-private-items --keep-going

# The presence guard exists because the failure it replaces was
# indistinguishable from a real finding: on a machine without the binary this
# recipe died with `/bin/sh: 1: shellcheck: not found` and `make: *** Error
# 127`, which reads exactly like a shell-lint failure in a script somebody just
# edited (#3615). It fails rather than skipping — a gate step that can no-op is
# a gate step that will, and a green tick over an unrun check is the one
# outcome the gate exists to prevent. The MESSAGE carries the
# distinction, never the exit code: GNU make normalises any recipe failure to
# its own status 2, so no caller can tell "could not run" from "ran and found
# something" by the number -- measured, a real finding exits the recipe 1 and
# make still reports 2. The lint invocation below is byte-identical to what it
# always was, so a machine that has shellcheck sees no change at all.
# Covered by scripts/test-shellcheck-guard.sh (`make shellcheck-guard-test`),
# which pins both halves: the notice on an absent binary, and that a present
# one is still invoked with its argv intact and its findings still fatal.
.PHONY: shellcheck
shellcheck: ## Lint install.sh, scripts/*.sh, and .githooks/* (#916)
	@command -v shellcheck >/dev/null 2>&1 || { \
	  printf '%s\n' \
	    'shellcheck: UNAVAILABLE — THIS STEP DID NOT RUN.' \
	    '' \
	    '  shellcheck is not on PATH. No shell script was linted, so this is' \
	    '  NOT a lint finding: nothing was checked, and nothing has been' \
	    '  established about install.sh, scripts/*.sh, scripts/lib/*.sh or' \
	    '  .githooks/*.' \
	    '' \
	    '  Install it, then re-run `make shellcheck`:' \
	    '' \
	    '    apt-get install -y shellcheck   # Debian/Ubuntu (most dev containers)' \
	    '    brew install shellcheck         # macOS' \
	    '    dnf install -y ShellCheck       # Fedora' \
	    '' \
	    '  ./scripts/setup-dev-env.sh --check reports it alongside the rest of' \
	    '  the tooling the gate needs. CI runs the same lint on a runner where' \
	    '  shellcheck is preinstalled (.github/workflows/ci.yml), so a missing' \
	    '  binary here is a gap in this machine, never a red tree.' >&2; \
	  exit 2; }
	shellcheck install.sh scripts/*.sh scripts/lib/*.sh .githooks/*

# Deliberately not part of `gate`: it needs a Docker daemon, which the gate
# must not. CI runs the same two commands (.github/workflows/docker-serve.yml).
.PHONY: serve-image
serve-image: ## Build the stella-serve image and smoke the container (needs Docker, #635)
	docker build -f packaging/docker/Dockerfile.serve -t stella-serve:ci .
	@./scripts/smoke-serve-image.sh stella-serve:ci

# The four tiers of the gate, from cheapest to dearest. Each is a superset of
# the one above it, and only the compile tiers honour CARGO_SCOPE.
#
#   guards-fast  the toolchain-free guards + the lock resolve + rustfmt.
#                Nothing compiles at all.
#   guards       ...plus wire-schema, whose two schema exporters do compile.
#   check        ...plus clippy. The graduated fallback: a reduced gate, not no gate.
#   gate         ...plus rustdoc and the test suite. What CI runs, unscoped.
#
# `guards-fast` exists for the one push shape where `guards` is provably
# wasted work: a diff that reaches no crate AND cannot have touched the wire
# contract — a website-only or workflow-only push. Every guard in
# GATE_GUARDS_FAST is a shell script over the tree, and GATE_NO_BUILD resolves
# (`cargo metadata --locked`) or parses (`cargo fmt --check`) rather than
# building, so this rung needs no build at all. The pre-push
# hook picks between the two by grepping the pushed diff (#1439); wire-schema
# is not optional, it is *conditional*, and the condition is in one place.
.PHONY: guards-fast
guards-fast: $(GATE_GUARDS_FAST) $(GATE_NO_BUILD) ## Toolchain-free guards + lock resolve + fmt (no cargo build at all)

.PHONY: guards
guards: $(GATE_GUARDS) $(GATE_NO_BUILD) ## Global guards + lock resolve + fmt only (nothing compiles beyond the wire-schema exporters; nothing to scope)

.PHONY: gate
gate: $(GATE_STEPS) ## Full CI gate: guards + rustdoc + fmt-check + clippy + test

# Consumed by scripts/check-gate-parity.sh. Printing the variable is the whole
# point: a guard that re-parsed this Makefile with a regex would be one more
# hand-maintained copy of the thing it is guarding.
.PHONY: print-gate-steps
print-gate-steps:
	@echo $(GATE_STEPS)

.PHONY: gate-parity
gate-parity: ## Assert AGENTS.md and CONTRIBUTING.md list the real gate steps (#1437)
	@./scripts/check-gate-parity.sh

.PHONY: bench-suites
bench-suites: ## Assert `make bench-test` runs every suite bench.yml runs, by derivation (#2847)
	@./scripts/check-bench-suites.sh

.PHONY: left-behind
left-behind: ## Assert every TODO/FIXME/XXX/HACK in code names a tracking issue (#1454)
	@./scripts/check-left-behind.sh

.PHONY: left-behind-update
left-behind-update: ## Regenerate the left-behind baseline (it should stay empty)
	@./scripts/check-left-behind.sh --update

.PHONY: role-names
role-names: ## Assert the agent-config role names match across Rust, Python and JS (#1449)
	@./scripts/check-role-names.sh

.PHONY: stat-portability
stat-portability: ## Assert file identity is read through MetadataExt, not a raw libc::stat (#1758)
	@./scripts/check-stat-portability.sh

.PHONY: module-reachability
module-reachability: ## Assert every .rs under a crate's src/ is reachable from its crate root (#1750)
	@python3 ./scripts/check-module-reachability.py

.PHONY: module-reachability-test
module-reachability-test: ## Test the module-reachability walker (hermetic; not part of `gate`)
	./scripts/test-module-reachability.sh

.PHONY: god-files
god-files: ## Assert AGENTS.md and the crate READMEs name the baselined god files (#1435)
	@./scripts/check-god-files.sh

.PHONY: lockfile-sync
lockfile-sync: ## Assert Cargo.lock resolves against the manifests as committed (#3332)
	@./scripts/check-lockfile-sync.sh

.PHONY: lockfile-sync-test
lockfile-sync-test: ## Test the lockfile guard against synthetic skewed workspaces (hermetic; not part of `gate`)
	./scripts/test-lockfile-sync.sh

.PHONY: release-lockfile-test
release-lockfile-test: ## Test that the release version stamp leaves Cargo.lock resolvable, new members included (hermetic; not part of `gate`)
	./scripts/test-release-lockfile.sh

# Deliberately not a gate step: it judges the MERGED tree, which is a question
# no pre-merge run can answer. .github/workflows/main-canary.yml is where it
# runs for real; this target is here so the logic can be exercised by hand.
.PHONY: main-canary
main-canary: ## Ask whether main still composes green (check only; no issue is filed)
	@./scripts/main-canary.sh

.PHONY: main-canary-test
main-canary-test: ## Test the post-merge canary, announcements included (hermetic; not part of `gate`)
	./scripts/test-main-canary.sh

.PHONY: check
check: $(GATE_GUARDS) $(GATE_NO_BUILD) lint ## Reduced pre-push gate: every guard + lock resolve + fmt + clippy, no rustdoc and no tests

.PHONY: impacted
impacted: ## Print the cargo scope for a diff (RANGE=origin/main..HEAD)
	@./scripts/impacted-crates.sh $(if $(RANGE),--range $(RANGE),--range origin/main..HEAD)

.PHONY: impacted-test
impacted-test: ## Test the gate-scoping script (hermetic; not part of `gate`)
	./scripts/test-impacted-crates.sh

.PHONY: self-driving-test
# Pins STELLA_BIN rather than letting the harness locate one. Its own
# `locate_stella` prefers target/release over target/debug, so a stale release
# build silently wins over the code under test — three cases went red against a
# months-old binary that predated the command they exercised, and the harness
# does not say which binary it measured (#1753). The gate must test what the
# gate just built.
self-driving-test: ## Test the self-driving control logic — digest, AIMD, aperture, run lifecycle (hermetic)
	cargo build -q -p stella-cli --bin stella
	STELLA_BIN="$(CURDIR)/target/debug/stella" ./scripts/test-self-driving.sh

.PHONY: smoke-artifact-test
smoke-artifact-test: ## Test the release-artifact smoke gate against synthetic broken artifacts (hermetic; not part of `gate`)
	./scripts/test-smoke-artifact.sh

.PHONY: automerge-nudge-test
automerge-nudge-test: ## Test which PR the auto-merge nudge picks (hermetic; not part of `gate`)
	./scripts/test-automerge-nudge.sh

.PHONY: file-size-test
file-size-test: ## Test the file-size ratchet's language coverage and its change-relative judgement (hermetic; not part of `gate`)
	./scripts/test-file-size.sh

.PHONY: guard-sigpipe-test
guard-sigpipe-test: ## Test that the gate guards survive a reader that closes their pipe early (#1815; hermetic; not part of `gate`)
	./scripts/test-guard-sigpipe.sh

.PHONY: changelog-roll-test
changelog-roll-test: ## Test which releases get a CHANGELOG.md section (hermetic; not part of `gate`)
	./scripts/test-changelog-roll.sh

.PHONY: releases-published
releases-published: ## Assert every v* tag older than the grace window has a published release (#1464)
	@./scripts/check-releases-published.sh

.PHONY: releases-baseline-update
releases-baseline-update: ## Grandfather the tags that shipped nothing and never will (review the diff!)
	@./scripts/check-releases-published.sh --update
	@git --no-pager diff --stat -- scripts/unpublished-tags-baseline.txt || true

.PHONY: releases-published-test
releases-published-test: ## Test the tag/release reconciliation rule (hermetic; not part of `gate`)
	./scripts/test-releases-published.sh

.PHONY: hooks
hooks: ## Install the pre-push gate hook (runs `make gate`, scoped to the diff, on every push)
	git config core.hooksPath .githooks
	@chmod +x .githooks/* 2>/dev/null || true
	@printf '\033[32m✔ hooks installed\033[0m — pre-push now runs the fmt+clippy+rustdoc+test gate.\n'
	@printf '  Catches a red gate on your machine instead of ten minutes later in CI.\n'
	@printf '  Compile tiers are scoped to the crates your diff reaches (#1135); pushes to\n'
	@printf '  main and tag pushes always run the whole workspace.\n'
	@printf '  Step down a rung: \033[36mGATE=fast git push\033[0m (guards+fmt+clippy, no tests),\n'
	@printf '  \033[36mGATE=full git push\033[0m (whole workspace), \033[36mSKIP_GATE=1 git push\033[0m (bypass).\n'

.PHONY: dev-env
dev-env: ## Set this worktree up for development (per-worktree STELLA_HOME + target dir, tool check)
	./scripts/setup-dev-env.sh

.PHONY: dev-env-check
dev-env-check: ## Report the development environment without writing anything
	./scripts/setup-dev-env.sh --check

.PHONY: dev-env-prune
dev-env-prune: ## Reclaim per-worktree caches whose worktree is gone
	./scripts/setup-dev-env.sh --prune

.PHONY: dev-env-test
dev-env-test: ## Test the dev-env scripts (hermetic; not part of `gate`)
	./scripts/test-dev-env.sh

.PHONY: docs
docs: ## Build rustdoc for the workspace (skip dep docs)
	cargo doc --workspace --no-deps

.PHONY: deny
deny: ## cargo deny: advisories, dependency bans, source provenance, licenses
	cargo deny check advisories bans sources licenses

.PHONY: supply-chain
supply-chain: deny ## Run the supply-chain check (alias for `deny`; see #919)

CARGO_WATCH := $(shell command -v cargo-watch 2>/dev/null)

.PHONY: watch
watch: ## Watch: re-run workspace tests on every save
ifeq ($(CARGO_WATCH),)
	$(error cargo-watch not installed — run: cargo install cargo-watch)
else
	cargo watch -x 'test --workspace'
endif

.PHONY: watch-core
watch-core: ## Watch: re-test stella-core on every save
ifeq ($(CARGO_WATCH),)
	$(error cargo-watch not installed — run: cargo install cargo-watch)
else
	cargo watch -x 'test -p stella-core'
endif

.PHONY: watch-lint
watch-lint: ## Watch: re-run clippy on every save
ifeq ($(CARGO_WATCH),)
	$(error cargo-watch not installed — run: cargo install cargo-watch)
else
	cargo watch -x 'clippy --workspace --all-targets -- -D warnings'
endif

.PHONY: watch-fix
watch-fix: ## Watch: auto-fix clippy + format on every save
ifeq ($(CARGO_WATCH),)
	$(error cargo-watch not installed — run: cargo install cargo-watch)
else
	cargo watch -x 'clippy --fix --allow-dirty --workspace --all-targets -- -D warnings' -x 'fmt'
endif

# The normal way to release is to push a tag and let .github/workflows/release.yml
# build, ATTEST and publish it. These targets drive scripts/release.sh, which is
# the degraded local path: it cannot mint a build-provenance attestation (that
# needs an Actions OIDC token), so everything it publishes is checksum-only, and
# its Linux tarballs are zig cross-builds rather than CI's native ones. The
# script refuses without ALLOW_UNATTESTED=1; these targets deliberately do NOT
# set it for you. See RELEASING.md.
.PHONY: release
release: ## Cut a release LOCALLY, unattested (default: patch; BUMP=minor|major). Prefer pushing a tag
	scripts/release.sh $(BUMP)

.PHONY: release-patch
release-patch: ## Cut a patch release locally, unattested (0.1.0 -> 0.1.1)
	scripts/release.sh patch

.PHONY: release-minor
release-minor: ## Cut a minor release locally, unattested (0.1.0 -> 0.2.0)
	scripts/release.sh minor

.PHONY: release-major
release-major: ## Cut a major release locally, unattested (0.1.0 -> 1.0.0)
	scripts/release.sh major

.PHONY: clean
clean: ## Remove all build artifacts
	cargo clean

.PHONY: reap-agents
reap-agents: ## List orphaned stella agents/tool-subprocesses idle 20m+ (dry run)
	scripts/reap-agents.sh --dry-run --verbose

.PHONY: reap-agents-kill
reap-agents-kill: ## Kill orphaned stella agents/tool-subprocesses idle 20m+ (asks first)
	scripts/reap-agents.sh

# The arena server is launched detached on purpose, so it outlives the terminal
# that started it — and therefore accumulates. `kill-arena` sends SIGTERM, which
# stops the server WITHOUT running serve()'s KeyboardInterrupt handler, so
# in-flight matches keep running and keep writing their artifacts. Pass
# `ARENA_ARGS=--cancel` for the SIGINT path that also cancels them. Both
# scripts carry the full reasoning in their headers.
.PHONY: kill-arena
kill-arena: ## Stop every arenabench server (in-flight matches survive; ARENA_ARGS=--cancel to end them)
	scripts/arena-kill.sh $(ARENA_ARGS)

#
# `ARENA_PORT` rather than a bare `PORT`: make inherits the environment as make
# variables, and `PORT` is exported by enough dev tooling that the generic name
# would silently pin a port nobody chose.
.PHONY: run-arena
run-arena: ## Launch a detached, Stella-wired arenabench server (ARENA_PORT= to pin; else first free from 8900)
	scripts/arena-run.sh $(if $(ARENA_PORT),--port $(ARENA_PORT),) $(ARENA_ARGS)

# The companion to kill-arena: that target deliberately spares every seat,
# because a seat orphaned by stopping an arena is still grading normally. This
# one is for a seat whose owner died for real. It judges liveness from the
# process tree — ppid, CPU movement, and what is running inside the seat's own
# containers — never from an arena's HTTP view, which cannot see a match
# started by `arenabench run` at all (#2326).
.PHONY: reap-seats
reap-seats: ## List abandoned harbor benchmark seats and their containers (dry run)
	scripts/reap-seats.sh --dry-run --verbose

.PHONY: reap-seats-kill
reap-seats-kill: ## Kill abandoned harbor seats and remove their task containers (asks first)
	scripts/reap-seats.sh

# A contest is a head-to-head on the fixed TB2.1 panel, and its arguments are
# make variables rather than positional words because make has no positional
# arguments — `make contest foo=1` is the native spelling, and a positional
# hack would fight the tool for no gain.
.PHONY: contest
contest: ## Stella vs Claude Code (num_tasks= versus_model= max_throughput= [target=cloud|local] [ref=])
	num_tasks="$(num_tasks)" versus_model="$(versus_model)" \
	max_throughput="$(max_throughput)" target="$(target)" ref="$(ref)" \
	attempts="$(attempts)" out="$(out)" extra="$(extra)" \
	scripts/arena-contest.sh

# The match-launching counterpart to run-arena, which launches the server. A
# positional `match=` rather than a bare word for the same reason `contest`
# takes make variables: make has no positional arguments.
#
# `ARENA_ARGS` is the escape hatch for everything this target does not name —
# --inject-all, --max-behind, --env-file, and anything after `--` bound for
# `arenabench run` itself.
.PHONY: run-match
run-match: ## Launch one arenabench match, credentialled from ~/.env.global.local (match=path/to.toml [ARENA_ARGS=…])
	@[ -n "$(match)" ] || { echo "usage: make run-match match=arenabench/matches/<name>.toml"; exit 2; }
	scripts/arena-local.sh $(match) $(ARENA_ARGS)

.PHONY: arena-scripts-test
arena-scripts-test: ## Test the arena start/stop/reap/local scripts — argv self-match, ancestry, liveness, credential seeding (hermetic; not part of `gate`)
	./scripts/test-arena-scripts.sh

# The supply-chain step gates on the TOOL being present, not on its exit code:
# a missing cargo-deny soft-skips with a message, but a real
# advisory/license/vulnerability failure from an installed tool fails the
# target. (The old `cmd || printf` form swallowed genuine failures too.)
.PHONY: audit
audit: ## Run full codebase audit (clippy, tests, supply-chain, dead-code scan)
	@printf '\033[1m=== Clippy ===\033[0m\n'
	cargo clippy --workspace --all-targets -- -D warnings
	@printf '\n\033[1m=== Tests ===\033[0m\n'
	cargo test --workspace
	@printf '\n\033[1m=== Supply chain ===\033[0m\n'
	@if command -v cargo-deny >/dev/null 2>&1; then \
		cargo deny check advisories bans sources licenses; \
	else \
		printf '  \033[33mcargo-deny not installed — skipping (cargo install cargo-deny)\033[0m\n'; \
	fi
	@printf '\n\033[1m=== Unused dependencies ===\033[0m\n'
	@# Same shape as the two steps above, and for the same reason: gate on the
	@# TOOL being present, not on its exit code. The old
	@# `cargo udeps … 2>/dev/null || printf "not installed"` form reported
	@# "not installed" for every outcome — a genuinely unused dependency, a
	@# compile error, and a missing binary were indistinguishable, and the
	@# `2>/dev/null` threw away the one message that could tell them apart.
	@# cargo-udeps needs a nightly toolchain (it reads unstable rustc output),
	@# so the invocation is explicit rather than relying on the pinned default.
	@if ! command -v cargo-udeps >/dev/null 2>&1; then \
		printf '  \033[33mcargo-udeps not installed — run: cargo install cargo-udeps\033[0m\n'; \
	elif ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then \
		printf '  \033[33mcargo-udeps needs nightly — run: rustup toolchain install nightly\033[0m\n'; \
	else \
		cargo +nightly udeps --workspace --all-targets; \
	fi
	@printf '\n\033[32m✔ Audit complete.\033[0m\n'
