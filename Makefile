# Stella — developer convenience targets.
# Run `make help` for the full list.

BUMP ?= patch

# `make record-demo` knobs — see scripts/record-demo.sh for the full set.
LIMIT ?= 0
TARGET ?= 60

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
lint: ## Run clippy with -D warnings (CI gate)
	cargo clippy --workspace --all-targets -- -D warnings

.PHONY: fix
fix: ## Auto-fix clippy lints + format
	cargo clippy --fix --allow-dirty --workspace --all-targets -- -D warnings
	cargo fmt

.PHONY: test
test: ## Run the full test suite (all crates)
	cargo test --workspace

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
	@git --no-pager diff --stat -- stella-pipeline/tests/fixtures/golden || true
	@echo "Golden trajectories re-recorded. A non-empty diff above is a change to"
	@echo "the observable event contract — review it as such before committing."

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

.PHONY: action-pins
action-pins: ## Assert every workflow `uses:` is pinned to a commit SHA (#648)
	@./scripts/check-action-pins.sh

.PHONY: cargo-install-pins
cargo-install-pins: ## Assert every workflow `cargo install` names an exact version (#915)
	@./scripts/check-cargo-install-pins.sh

.PHONY: license-allowlist-parity
license-allowlist-parity: ## Assert deny.toml and dependency-review.yml agree on allowed licenses (#920)
	@./scripts/check-license-allowlist-parity.sh

.PHONY: doc-citations
doc-citations: ## Assert docs citations resolve and none cite by line number (#652, #561)
	@./scripts/check-doc-citations.sh

.PHONY: invariants
invariants: ## Assert the architectural invariants have one home and stable numbering (#630)
	@./scripts/check-invariants.sh

.PHONY: wire-schema
wire-schema: ## Assert docs/wire/ still describes the AgentEvent wire format (#971)
	@./scripts/check-wire-schema.sh

.PHONY: wire-schema-update
wire-schema-update: ## Regenerate docs/wire/ after an AgentEvent change (commit the diff!)
	@./scripts/export-agentevent-schema.sh

.PHONY: file-size
file-size: ## Assert no new .rs file exceeds the 1500-line ratchet (#629)
	@./scripts/check-file-size.sh

.PHONY: file-size-update
file-size-update: ## Retighten the 1500-line ratchet baseline (run after splitting a file)
	@./scripts/check-file-size.sh --update

.PHONY: doc-warnings
doc-warnings: ## Assert rustdoc is clean workspace-wide (#634)
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

.PHONY: shellcheck
shellcheck: ## Lint install.sh, scripts/*.sh, and .githooks/* (#916)
	shellcheck install.sh scripts/*.sh .githooks/*

# Deliberately not part of `gate`: it needs a Docker daemon, which the gate
# must not. CI runs the same two commands (.github/workflows/docker-serve.yml).
.PHONY: serve-image
serve-image: ## Build the stella-serve image and smoke the container (needs Docker, #635)
	docker build -f packaging/docker/Dockerfile.serve -t stella-serve:ci .
	@./scripts/smoke-serve-image.sh stella-serve:ci

.PHONY: gate
gate: no-scratch action-pins cargo-install-pins license-allowlist-parity shellcheck doc-citations invariants file-size wire-schema doc-warnings format-check lint test ## Full CI gate: no-scratch + action-pins + cargo-install-pins + license-allowlist-parity + shellcheck + doc-citations + invariants + file-size + wire-schema + rustdoc + fmt-check + clippy + test

.PHONY: check
check: no-scratch action-pins cargo-install-pins license-allowlist-parity shellcheck invariants file-size format-check lint ## Fast pre-push check (scratch + pins + license parity + shellcheck + invariants + file-size + fmt + clippy, no tests)

.PHONY: hooks
hooks: ## Install the pre-push gate hook (runs `make gate` on every push)
	git config core.hooksPath .githooks
	@chmod +x .githooks/* 2>/dev/null || true
	@printf '\033[32m✔ hooks installed\033[0m — pre-push now runs the fmt+clippy+test gate.\n'
	@printf '  Catches a red gate on your machine, and is the only gate running when\n'
	@printf '  Actions is unavailable (an org billing hold has happened before).\n'
	@printf '  Bypass in emergencies: \033[36mSKIP_GATE=1 git push\033[0m (or \033[36mgit push --no-verify\033[0m).\n'

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

.PHONY: vuln-scan
vuln-scan: ## cargo audit: security vulnerability scan
	cargo audit

.PHONY: supply-chain
supply-chain: deny vuln-scan ## Run both supply-chain checks

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

# The supply-chain steps gate on the TOOL being present, not on its exit code:
# a missing cargo-deny/cargo-audit soft-skips with a message, but a real
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
	@if command -v cargo-audit >/dev/null 2>&1; then \
		cargo audit; \
	else \
		printf '  \033[33mcargo-audit not installed — skipping (cargo install cargo-audit)\033[0m\n'; \
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
