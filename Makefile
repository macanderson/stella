# Stella — developer convenience targets.
# Run `make help` for the full list.

BUMP ?= patch

# `make record-demo` knobs — see scripts/record-demo.sh for the full set.
LIMIT ?= 0
TARGET ?= 60

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
                    role-names
GATE_GUARDS := $(GATE_GUARDS_FAST) wire-schema

# The whole gate, in order, as one name. `gate` below is defined *from* this
# variable rather than restating it, and `make print-gate-steps` prints it —
# which is what lets scripts/check-gate-parity.sh hold AGENTS.md and
# CONTRIBUTING.md to the real list instead of a hand-copied one. Both documents
# had already re-rotted twice, in the same direction each time: a guard was
# added here and the prose kept the old count (#1437).
GATE_STEPS := $(GATE_GUARDS) doc-warnings format-check lint test

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

.PHONY: record-golden
record-golden: ## Re-record the golden replay trajectories (review the fixture diff!)
	STELLA_REFRESH_GOLDEN=1 cargo test -p stella-pipeline --lib golden
	@git --no-pager diff --stat -- crates/stella-pipeline/tests/fixtures/golden || true
	@echo "Golden trajectories re-recorded. A non-empty diff above is a change to"
	@echo "the observable event contract — review it as such before committing."

# Prompt caching has no stale-read failure mode — a hit and a miss return the
# same tokens — so a broken cache is invisible until it shows up on an invoice.
# What is checkable is that the prompt still has the shape caching needs, and
# the golden manifests record exactly that (a `cache_zone` per block). This is
# the fast local loop; `make test` runs it too, as part of the workspace suite.
.PHONY: cache-correctness
cache-correctness: ## Assert prompt-cache shape hasn't drifted on the golden fixtures (#267/#269, receipts spec §7)
	cargo test -p stella-pipeline --test cache_correctness
	cargo test -p stella-cli --bin stella stats::tests::the_low_hit_rate_bar

.PHONY: record-demo
record-demo: ## Record a terminal timelapse (LIMIT=mins TARGET=secs CMD="..."; defaults to the multi-hour marathon)
	./scripts/record-demo.sh --limit $(LIMIT) --target $(TARGET) $(if $(CMD),-- bash -c '$(CMD)',)

.PHONY: bench-test
bench-test: ## Test the Python benchmark tooling (TB2.1 adapter + analyzer)
	cd bench/harbor_adapter && uv sync --locked --extra dev && uv run --no-sync pytest -q
	cd bench/terminal_bench_analysis && uv sync --locked --extra dev && uv run --no-sync pytest -q

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

.PHONY: doc-warnings
doc-warnings: ## Assert rustdoc is clean workspace-wide (#634; CARGO_SCOPE to narrow)
	RUSTDOCFLAGS="-D warnings" cargo doc $(CARGO_SCOPE) --no-deps

.PHONY: shellcheck
shellcheck: ## Lint install.sh, scripts/*.sh, and .githooks/* (#916)
	shellcheck install.sh scripts/*.sh .githooks/*

# Deliberately not part of `gate`: it needs a Docker daemon, which the gate
# must not. CI runs the same two commands (.github/workflows/docker-serve.yml).
.PHONY: serve-image
serve-image: ## Build the stella-serve image and smoke the container (needs Docker, #635)
	docker build -f packaging/docker/Dockerfile.serve -t stella-serve:ci .
	@./scripts/smoke-serve-image.sh stella-serve:ci

# The four tiers of the gate, from cheapest to dearest. Each is a superset of
# the one above it, and only the compile tiers honour CARGO_SCOPE.
#
#   guards-fast  the toolchain-free guards + rustfmt. Nothing compiles at all.
#   guards       ...plus wire-schema, whose two schema exporters do compile.
#   check        ...plus clippy. The graduated fallback: a reduced gate, not no gate.
#   gate         ...plus rustdoc and the test suite. What CI runs, unscoped.
#
# `guards-fast` exists for the one push shape where `guards` is provably
# wasted work: a diff that reaches no crate AND cannot have touched the wire
# contract — a website-only or workflow-only push. Every guard in
# GATE_GUARDS_FAST is a shell script over the tree, and `cargo fmt --check`
# parses rather than builds, so this rung needs no build at all. The pre-push
# hook picks between the two by grepping the pushed diff (#1439); wire-schema
# is not optional, it is *conditional*, and the condition is in one place.
.PHONY: guards-fast
guards-fast: $(GATE_GUARDS_FAST) format-check ## Toolchain-free guards + fmt (no cargo build at all)

.PHONY: guards
guards: $(GATE_GUARDS) format-check ## Global guards + fmt only (nothing compiles beyond the wire-schema exporters; nothing to scope)

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

.PHONY: left-behind
left-behind: ## Assert every TODO/FIXME/XXX/HACK in code names a tracking issue (#1454)
	@./scripts/check-left-behind.sh

.PHONY: left-behind-update
left-behind-update: ## Regenerate the left-behind baseline (it should stay empty)
	@./scripts/check-left-behind.sh --update

.PHONY: role-names
role-names: ## Assert the agent-config role names match across Rust, Python and JS (#1449)
	@./scripts/check-role-names.sh

.PHONY: god-files
god-files: ## Assert AGENTS.md and the crate READMEs name the baselined god files (#1435)
	@./scripts/check-god-files.sh

.PHONY: check
check: $(GATE_GUARDS) format-check lint ## Reduced pre-push gate: every guard + fmt + clippy, no rustdoc and no tests

.PHONY: impacted
impacted: ## Print the cargo scope for a diff (RANGE=origin/main..HEAD)
	@./scripts/impacted-crates.sh $(if $(RANGE),--range $(RANGE),--range origin/main..HEAD)

.PHONY: impacted-test
impacted-test: ## Test the gate-scoping script (hermetic; not part of `gate`)
	./scripts/test-impacted-crates.sh

.PHONY: self-driving-test
self-driving-test: ## Test the self-driving control logic — digest, AIMD, aperture, run lifecycle (hermetic; not part of `gate`)
	./scripts/test-self-driving.sh

.PHONY: smoke-artifact-test
smoke-artifact-test: ## Test the release-artifact smoke gate against synthetic broken artifacts (hermetic; not part of `gate`)
	./scripts/test-smoke-artifact.sh

.PHONY: automerge-nudge-test
automerge-nudge-test: ## Test which PR the auto-merge nudge picks (hermetic; not part of `gate`)
	./scripts/test-automerge-nudge.sh

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
