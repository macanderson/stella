<p align="center">
  <picture>
    <source srcset="docs/brand/logo/svg/lockup-color-light.svg">
    <img src="docs/brand/logo/svg/lockup-color-light.svg" alt="Stella" width="300">
  </picture>
</p>

<p align="center"><strong>A coding agent that doesn't lie to you</strong></p>
<p align="center">Open Source · Rust · BYOK · No Phone Home</p>

<p align="center">
  <a href="https://github.com/macanderson/stella/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/macanderson/stella/ci.yml?branch=main&style=flat-square&logo=github&label=ci" alt="CI status"></a>
  <a href="https://github.com/macanderson/stella/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/macanderson/stella/release.yml?style=flat-square&logo=github&label=release" alt="Release status"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-080A0F?style=flat-square" alt="License"></a>
  <img src="https://img.shields.io/badge/rust-1.90%2B-080A0F?style=flat-square&logo=rust&logoColor=F5C145" alt="Rust 1.90+">
  <img src="https://img.shields.io/badge/providers-9%20%2B%20local-080A0F?style=flat-square" alt="9 providers + local">
</p>

<p align="center">
  <a href="https://stella.oxagen.sh"><b>Website</b></a> ·
  <a href="https://stella.oxagen.sh/docs"><b>Docs</b></a> ·
  <a href="https://stella.oxagen.sh/docs/getting-started/installation"><b>Quickstart</b></a>
</p>

Ship deterministically verified code fully autonomously with Stella, a self-improving
next generation coding agent. Stella is an open-source, bring-your-own-key (BYOK)
coding agent that runs in your terminal. It supports nine hosted model
providers plus any local OpenAI-compatible server, keeps canonical telemetry
in a local SQLite database, and enforces a hard per-run budget. Telemetry leaves
your machine only if you configure it to, by one of exactly two explicit paths:
an enrolled Oxagen Enterprise managed install, which may export only a minimal
operational rollup under the governed boundary described below; or a `drain`
block in `~/.stella/cloud.json`, which `stella cloud sync` uses to POST staged
rows to an org intake you name. Neither exists in a default install. It is built
in Rust as a workspace of focused crates.

## Features

- **BYOK, auto-detected** — Set one provider's API key and Stella detects it.
  Pin a specific model per run or shell with `--model`.
- **Deterministic definition of done** — `verify_done` replays your new test
  files against the previous code in a shadow worktree at `git HEAD`; the test
  must fail there and pass on your change. A green suite alone is not accepted.
- **Single-threaded engine** — One deterministic step loop: plan, fan tools out
  in parallel, observe, compact if noisy, repeat. No coordinator or multi-agent
  swarm.
- **Prompt-cache-native memory** — Lessons saved with `save_memory` load once at
  session start into a byte-stable system prompt (~0.1× input cost).
- **Code graph** — A tree-sitter symbol/import index (Rust, TS/TSX/JS, Python,
  Go, Java, C, PHP, SQL) queried by the agent and the `stella graph` command
  instead of grepping.
- **Local-first telemetry** — Executions, events, token/cost telemetry, and the
  files-touched ledger stay canonical in `.stella/private/store.db`.
  Community/default sends none of it anywhere. Only explicitly enrolled Oxagen
  Enterprise managed mode can derive a closed, content-free operational rollup.
- **Budget enforcement** — A `--budget` flag aborts cleanly between steps, never
  mid-tool.
- **Goal & fleet modes** — `goal` works in judged rounds; `fleet` fans a task DAG
  out to parallel workers that share one tree under cooperative file claims, or
  take their own git worktree when a task opts in.
- **Lifecycle hooks** — Shell-command hooks (`SessionStart`, `PreToolUse`,
  `PostToolUse`) configurable in `settings.json`.

## Prerequisites

- **macOS or Linux**, `x86_64` or `arm64`.
- Private persistence currently depends on Unix owner/mode and no-follow
  primitives. Non-Unix builds fail closed for sensitive state writes; Windows
  persistence is not currently supported or claimed.
- For prebuilt / Homebrew install: `curl`.
- For building from source: **Rust 1.90+** (via [rustup](https://rustup.rs)) and `git`.
  Building a clone of this repository uses the exact toolchain pinned in
  `rust-toolchain.toml` (currently 1.97.0) — rustup downloads it automatically on
  the first `cargo build`, so expect a one-time toolchain fetch.
- An API key for any supported provider, _or_ a local OpenAI-compatible model
  server (Ollama, vLLM, LM Studio, llama.cpp).
- Optional: [`ripgrep`](https://github.com/BurntSushi/ripgrep) and
  [`fd`](https://github.com/sharkdp/fd) on `PATH` (used by the `grep`/`glob`
  tools), and `gh` for the CI/issue tools.

## Install

**Prebuilt binary:**

```bash
curl -fsSL https://raw.githubusercontent.com/macanderson/stella/main/install.sh | sh
stella --version
```

The installer downloads the latest release tarball, verifies its SHA-256, and
falls back to `cargo install` when no prebuilt binary matches your platform.

**Homebrew:**

```bash
brew install macanderson/tap/stella
```

To build from source via Homebrew:
`brew install --build-from-source ./packaging/homebrew/stella.rb`.

**From cargo** (requires Rust 1.90+ and git):

```bash
cargo install --locked --git https://github.com/macanderson/stella stella-cli
stella --version
```

**From source:**

```bash
git clone https://github.com/macanderson/stella.git
cd stella
cargo build --release
./target/release/stella --version
```

### Not on crates.io

The `stella-*` crates are **not published to crates.io** — `publish = false` is
set once at `[workspace.package]` in the root `Cargo.toml` and inherited by every
member. (The [Context Graph Protocol](https://github.com/macanderson/context-graph-protocol)
crates are now exact-version registry dependencies, so the old git-dependency
blocker is gone; the workspace simply is not published as a crate set.)
The `cargo install --git …` command above is therefore the only supported cargo
path — dropping `--git` does **not** install this project.

## Set your API key

Stella is BYOK and auto-detects the provider from whichever keys you have set.

| Provider               | Env var                                       | Default model                                                                    |
| ---------------------- | --------------------------------------------- | -------------------------------------------------------------------------------- |
| **OpenRouter**         | `OPENROUTER_API_KEY`                          | `moonshotai/kimi-k3`                                                             |
| **Z.ai** (GLM)         | `ZAI_API_KEY`                                 | `glm-5.2`                                                                        |
| **Anthropic** (Claude) | `ANTHROPIC_API_KEY`                           | `claude-fable-5`                                                                 |
| **OpenAI** (GPT)       | `OPENAI_API_KEY`                              | `gpt-5.5`                                                                        |
| **xAI** (Grok)         | `XAI_API_KEY`                                 | `grok-4` — [retires 2026-08-15](https://stella.oxagen.sh/docs/api-providers/xai) |
| **DeepSeek**           | `DEEPSEEK_API_KEY`                            | `deepseek-chat`                                                                  |
| **Google Gemini**      | `GEMINI_API_KEY` (alias `GOOGLE_API_KEY`)     | `gemini-3-pro`                                                                   |
| **Google Vertex AI**   | `VERTEX_ACCESS_TOKEN` + `VERTEX_PROJECT_ID`   | `gemini-3-pro`                                                                   |
| **Amazon Bedrock**     | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` | `us.anthropic.claude-sonnet-4-5-20250929-v1:0`                                   |
| **Local**              | _none_ — pass `--base-url`                    | whatever your server hosts                                                       |

OpenRouter is checked first — its key is gateway-specific, so having one is a deliberate
choice rather than an accident of your shell — and it brings a whole default posture, not
just a default model: Kimi K3 driving at `xhigh` with thinking on, `anthropic/claude-opus-5`
judging, `z-ai/glm-5.2` triaging, all on the one key. Every field of that composes
underneath your own settings, so anything you configure wins. OpenRouter's slugs keep
their vendor namespace on the wire, so pinning one on the CLI needs both halves:
`--model openrouter/moonshotai/kimi-k3`.

```bash
export ANTHROPIC_API_KEY=your_key_here     # or OPENAI_API_KEY, GEMINI_API_KEY, …
```

Pin a provider/model per run or shell:

```bash
stella --model anthropic/claude-fable-5 run "refactor the database layer"
export STELLA_MODEL=openai/gpt-5.5
```

**Local / any OpenAI-compatible gateway** — no key required:

```bash
stella --model local/llama3.3 --base-url http://localhost:11434/v1 chat
```

**Z.ai GLM Coding Plan:** set `ZAI_GLM_CODING_PLAN=1` alongside `ZAI_API_KEY` to
route through the dedicated coding endpoint.

**Credential chain** (first hit wins): `--api-key` flag → provider env var →
`settings.json` `api_key` → `~/.stella/credentials.toml` → interactive prompt.

`credentials.toml` is written by
[`stella auth`](https://stella.oxagen.sh/docs/commands/auth) — `auth set <provider>`
stores a key (prompted and masked unless you pass `--key`/`--stdin`), `auth list`
shows every stored key redacted alongside the source that actually wins, and
`auth remove <provider>` deletes one. It never prints a secret value.

**Project `.env` files** — so keys can follow the project you're in, Stella
reads `.env`, `.env.local`, and `.env.<mode>.local` (e.g. `.env.production.local`)
from the working directory (or the nearest ancestor within the same git repo)
into the environment at startup, most-specific file first. Template files
(`.env.example`, `.env.sample`, `.env.dist`) and non-`.local` mode files
(`.env.production`) are never read. **Your live shell always wins** — a value
already exported (or `OPENROUTER_API_KEY=… stella …`) is never overwritten by a
file, so unset a stale export if you mean to switch. Disable the whole mechanism
with `STELLA_NO_ENV_FILE=1`; see what loaded with `STELLA_ENV_DEBUG=1`.

```bash
stella models    # list providers, models, and key status
stella config    # show the fully resolved configuration
```

### Custom providers via `settings.json`

Point Stella at any OpenAI-compatible (or Anthropic/Gemini-dialect) endpoint
without a code change, and override built-in defaults, from a `settings.json`:

| Scope       | Path                                                                                                                           | Wins over         |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------ | ----------------- |
| Project     | `<workspace>/.stella/settings.json`                                                                                            | org-managed, user |
| Org-managed | `/Library/Application Support/stella/settings.json` (macOS) · `/etc/stella/settings.json` (Linux) · `$STELLA_MANAGED_SETTINGS` | user              |
| User        | `~/.stella/settings.json`                                                                                                      | —                 |

```jsonc
{
  "providers": {
    // A brand-new provider: base_url is required, dialect defaults to
    // "openai-compatible" ("anthropic" and "gemini" also available).
    "together": {
      "name": "Together AI",
      "base_url": "https://api.together.xyz/v1",
      "api_key_env": "TOGETHER_API_KEY",
      "default_model": "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    },
    // Overriding a built-in's defaults (e.g. the Z.ai coding plan):
    "zai": {
      "base_url": "https://api.z.ai/api/coding/paas/v4",
    },
  },
}
```

Then: `stella --model together/meta-llama/Llama-3.3-70B-Instruct-Turbo run "…"`.
Prefer `api_key_env` over a literal `api_key` — settings files get committed.

> **Untrusted repos can't redirect your key.** A cloned repo's project-scope
> `.stella/settings.json` is untrusted: its credential-routing fields
> (`base_url`, `api_key`, `api_key_env`, and `mcp.registry_url`) are **ignored**
> unless you opt in with `STELLA_TRUST_PROJECT=1`, so a hostile repo can't
> silently point your real API key at its own server. Cosmetic fields
> (`name`, `default_model`, `dialect`) still apply; the user and org-managed
> scopes are always trusted. Project hooks are gated the same way, via
> `STELLA_PROJECT_HOOKS`.

### Agent engine config (`agent_engine_config`)

The engine runs four configurable agents — **default** (the interactive /
step-loop agent) and the pipeline's **worker**, **judge**, and **triage**.
The `agent_engine_config` object in the same `settings.json` scope chain
configures each one's model, gateway, system prompt, reasoning, and sampling
parameters — and in the Command Deck, `/settings` opens the SETTINGS tab,
whose engine-config editor covers all of it (`s` saves to user scope, `S` to
project scope; the per-agent model pickers offer `allowed_models`, falling
back to the catalog when that list is empty). There are no per-agent slash
commands — the SETTINGS tab is the one place models are configured.

```jsonc
{
  "agent_engine_config": {
    // Flat per-role models ("provider/slug", or a bare catalog slug).
    "default_model": "anthropic/claude-fable-5",
    "pipeline_worker_model": "zai/glm-5.2",
    "pipeline_judge_model": "openrouter/openai/gpt-5.5",
    "pipeline_triage_model": "deepseek/deepseek-chat",

    // The model vocabulary the TUI pickers offer and auto_mode selects from.
    "allowed_models": [
      "anthropic/claude-fable-5",
      "zai/glm-5.2",
      "openrouter/openai/gpt-5.5",
    ],

    // "on" = pick the judge automatically from allowed_models: prefer a
    // different model family than the worker's, then the highest catalog
    // price tier. You never worry about it.
    "auto_mode": "off",
    // "on" = per-agent effort is chosen for you (judge high, worker
    // medium, triage low), overriding any per-agent "effort".
    "effort_auto": "off",
    // "on" = thinking mode chosen for you (on everywhere except triage).
    "reasoning_auto": "off",

    // Per-agent deep config. Every field is optional — set it and it goes
    // on the wire; leave it out and the provider default applies.
    "agents": {
      "judge": {
        "provider": "openrouter", // gateway: the slug goes to THIS
        "model": "openai/gpt-5.5", // provider verbatim (BYOK per agent)
        "prompt": "You are a strict, evidence-first code judge.",
        "effort": "high", // low | medium | high | xhigh | max
        "reasoning": "on", // thinking mode on/off
        "params": {
          "temperature": 0.2,
          "top_p": 0.9,
          "top_k": 40,
          "frequency_penalty": 0.0,
          "presence_penalty": 0.0,
          "repetition_penalty": 1.0,
          "max_tokens": 4096,
          "seed": 7,
          "verbosity": "low", // OpenAI/Anthropic-family models
          "service_tier": "priority", // providers with tiered service
        },
      },
    },
  },
}
```

Precedence per agent: `--model` flag > `agents.<agent>.model` >
`pipeline_<agent>_model` > `default_model` > auto-detect. An agent's
`provider` field routes its slug through that gateway verbatim, so the
worker can run on your Anthropic key while the judge routes
`openai/gpt-5.5` through your OpenRouter key and triage hits Z.ai. Each
adapter forwards only the parameters its wire supports (`verbosity` and
`service_tier` are dropped where meaningless); reasoning maps to GLM's
`thinking`, OpenRouter's `reasoning`, Anthropic extended thinking (with an
effort-tiered budget), OpenAI `reasoning.effort`, and Gemini
`thinkingLevel`. Custom prompts replace the built-in base instructions;
workspace memories and rules still append. A judge/triage model whose
provider has no resolvable key degrades softly — the role rides the worker
and a notice says so.

## Usage

### Command index

The full subcommand surface. Every command also answers `stella <command> --help`;
each row links to its reference page on [stella.oxagen.sh](https://stella.oxagen.sh/docs/commands).

| Command                                                               | What it does                                                                                                      |
| --------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| [`run <prompt>`](https://stella.oxagen.sh/docs/commands/run)          | Send a one-shot prompt, non-interactive — the staged pipeline by default                                          |
| [`chat`](https://stella.oxagen.sh/docs/commands/chat)                 | Interactive session: the Command Deck TUI (also what a bare `stella` opens)                                       |
| [`resume [id]`](https://stella.oxagen.sh/docs/commands/resume)        | Reopen a durable past session exactly where it stood; `--list` browses them                                       |
| [`goal <goal>`](https://stella.oxagen.sh/docs/commands/goal)          | Work in judged rounds until a judge model confirms the goal is met                                                |
| [`monitor [target]`](https://stella.oxagen.sh/docs/commands/monitor)  | Watch a branch/PR's CI and fix failures until it is fully green                                                   |
| [`fleet <tasks…>`](https://stella.oxagen.sh/docs/commands/fleet)      | Fan tasks out to worker agents, wave-scheduled and recorded in a ledger                                           |
| [`init`](https://stella.oxagen.sh/docs/commands/init)                 | Infer this workspace's domain taxonomy and build the code-graph index                                             |
| [`graph <op> <target>`](https://stella.oxagen.sh/docs/commands/graph) | Query the code graph — definitions, references, imports, neighbors (offline)                                      |
| [`storage <cmd>`](https://stella.oxagen.sh/docs/commands/storage)     | Inspect the storage map: layers, namespaces, relations, fields, drift (offline)                                   |
| [`scripts <cmd>`](https://stella.oxagen.sh/docs/commands/scripts)     | List and run the project's package-manager scripts by canonical verb (offline)                                    |
| [`tools`](https://stella.oxagen.sh/docs/commands/tools)               | List every tool available this session; `--validate` checks custom manifests                                      |
| [`models`](https://stella.oxagen.sh/docs/commands/models)             | List configured providers and available models                                                                    |
| [`auth <cmd>`](https://stella.oxagen.sh/docs/commands/auth)           | Manage BYOK provider keys in `~/.stella/credentials.toml` — never prints a secret                                 |
| [`config`](https://stella.oxagen.sh/docs/commands/config)             | Show the fully resolved configuration                                                                             |
| [`mcp <cmd>`](https://stella.oxagen.sh/docs/commands/mcp)             | Manage MCP servers: search a registry, install, list, log in, show usage                                          |
| [`connect <cmd>`](https://stella.oxagen.sh/docs/commands/connect)     | Connect GitHub or Linear so the agent gains the issue toolset                                                     |
| [`memory <cmd>`](https://stella.oxagen.sh/docs/commands/memory)       | Inspect memories through the citation loop; promote one to a project rule                                         |
| [`stats`](https://stella.oxagen.sh/docs/commands/stats)               | Cost, tokens, and $/resolved task for **this** workspace                                                          |
| [`usage <cmd>`](https://stella.oxagen.sh/docs/commands/usage)         | The same numbers across **every** project, from the hub at `~/.stella/usage.db`                                   |
| [`inspect`](https://stella.oxagen.sh/docs/commands/inspect)           | Replay the exact context a past model call was sent, verified against its digests                                 |
| [`observe`](https://stella.oxagen.sh/docs/commands/observe)           | Serve the Observatory dashboard over local telemetry — loopback-only, read-only                                   |
| [`cloud <cmd>`](https://stella.oxagen.sh/docs/commands/cloud)         | Show or set the org/workspace identity that scopes replicated telemetry                                           |
| [`telemetry <cmd>`](https://stella.oxagen.sh/docs/telemetry)          | Inspect or flush the managed enterprise spool — off unless explicitly enrolled                                    |
| [`arena`](https://stella.oxagen.sh/docs/commands)                     | [arena-bench](https://github.com/macanderson/arena-bench) harness adapter — for benchmarking Stella, not using it |
| [`doctor`](https://stella.oxagen.sh/docs/commands/doctor)             | Diagnose the install: config, credentials, toolchain, and workspace state                                         |
| [`proposals <cmd>`](https://stella.oxagen.sh/docs/commands)           | Review the adaptive-context loop's pending proposals — keep, ignore, or retire                                    |
| [`version`](https://stella.oxagen.sh/docs/commands/version)           | Print the version and exit                                                                                        |

### Interactive chat (default)

```bash
stella            # or: stella chat
```

On a TTY this opens the **Command Deck** — a tabbed TUI (Session · Agents ·
Traces · Graph · Files · Skills · MCP) with PR-style diffs and an editable prompt
queue. `--accessible` (or `STELLA_ACCESSIBLE=1`) runs that same deck so a screen
reader can read it: inline on your own screen, each finished message into normal
scrollback exactly once, single-column panels, labelled rows instead of tables,
and a spoken line whenever you change tab, overlay, or focus. `--plain` (or
`STELLA_PLAIN=1`, or piped stdio) falls back to the line REPL.

**In-chat commands** — the Command Deck and the line REPL each implement their
own vocabulary, so the surface column says where a command actually works:

| Command                                                                                  | Surface     | Does                                                                                                                                |
| ---------------------------------------------------------------------------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `/help` `/clear`                                                                         | Deck + REPL | Show help · clear history                                                                                                           |
| `/models`                                                                                | Deck + REPL | List providers/models (`/models refresh` re-syncs the catalog)                                                                      |
| `/init` `/agents`                                                                        | Deck + REPL | Reindex the workspace · the installed custom agents (a tab in the deck)                                                             |
| `/files`                                                                                 | Deck + REPL | The Files-Touched ledger — `[C·R·U·D] path` per file (a panel in the REPL, the Files tab in the deck)                               |
| `/goal <text>`                                                                           | REPL only   | Work in judged rounds until the goal is met                                                                                         |
| `/config`                                                                                | REPL only   | Show resolved configuration                                                                                                         |
| `/rename <name>` `/color <name>`                                                         | REPL only   | Rename the tab · switch accent color                                                                                                |
| `/pipeline`                                                                              | Deck only   | Toggle witness-verified staged turns (see [the inference pipeline](https://stella.oxagen.sh/docs/inference-pipeline))               |
| `/settings` `/diff` `/graph` `/skills` `/mcp` `/mcp-search` `/sessions` `/context` `/inspect` `/inbox` | Deck only | Open the corresponding tab or overlay (`/settings` includes the engine-config editor)                             |
| `/export`                                                                                | Deck only   | Export session telemetry to a ZIP + HTML dashboard                                                                                  |
| `/exit` or `Ctrl-D`                                                                      | REPL only   | Exit (the deck exits with `Ctrl-C`)                                                                                                 |

### One-shot run

```bash
stella run "fix the failing test in src/auth.rs"
stella run "add a health check endpoint to the API"
```

### Goal mode

```bash
stella goal "the login flow has a passing e2e test and CI is green"
stella monitor main          # drive a branch/PR's CI to green as a judged goal
```

### Fleet mode

```bash
stella fleet "fix the flaky auth test" "tighten the CI cache key"   # two isolated tasks
stella fleet --plan .stella/fleet.toml --max-concurrency 2 --budget 5.0
```

Wave-scheduled by dependency and recorded in `.stella/private/fleet.db`. Workers
share the repository root by default, coordinated by cooperative file claims; a
task with `isolation = "isolated"` gets its own git worktree under
`.stella/worktrees/` on a `fleet/<slug>-<hash>` branch instead. A plan file is
the serde form of the fleet DAG: `[[tasks]]` entries with `id`, `title`,
`prompt`, optional `depends_on`, and `isolation`.

### Code graph queries

```bash
stella graph definitions run_turn     # where is this symbol defined?
stella graph importers src/auth.rs    # which files import it?
```

Built by `stella init`, answered offline, no API key needed.

### Project setup & introspection

```bash
stella init      # infer this workspace's domain taxonomy (.stella/domains.toml)
stella tools     # list every tool available to the agent this session
stella stats     # cost, tokens, and $/resolved task per provider/model
                 # (--format table|json|csv, --provider <id>)
stella inspect   # the exact context a past model call was sent, rebuilt from
                 # recorded receipts (--step N, --call-seq S, --format json)
```

### Global flags

`--model provider/id` · `--api-key` · `--base-url` · `--budget <usd>` ·
`--output-format text|json|stream-json` · `--accessible` · `--plain` ·
`--no-anim` (also as `STELLA_MODEL`, `STELLA_BASE_URL`, `STELLA_BUDGET`,
`STELLA_OUTPUT_FORMAT`, `STELLA_ACCESSIBLE`, `STELLA_PLAIN`, `STELLA_NO_ANIM`). All of them are registered with every
subcommand, so they parse before _or_ after the subcommand token. The `json` /
`stream-json` formats are for headless one-shot `stella run`; interactive
`chat` / `goal` / `monitor` modes render human-readable output. `stella run`
uses the staged pipeline by default; `--no-pipeline` falls back to the raw
step-loop. In pipeline mode, `--test-command <cmd>` arms deterministic
verification with your own test; without it an independent witness author
writes a failing test whose fail→pass flip proves the work
([the inference pipeline](https://stella.oxagen.sh/docs/inference-pipeline)).
Post-turn reflection remains enabled for one-shot text, JSON, and stream-JSON
runs. Ephemeral automation can suppress that additional model call explicitly
with `STELLA_DISABLE_REFLECTION=1`; the truthy values `true`, `yes`, and `on`
are also accepted case-insensitively.

## Built-in tools

| Tool                                                                                                                                     | Description                                                                                                                                                                                                                                                                                      |
| ---------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `read_file` · `write_file` · `edit_file` · `delete_file`                                                                                 | File CRUD with surgical exact-substring edits                                                                                                                                                                                                                                                    |
| `apply_edits`                                                                                                                            | One transactional batch of exact-substring edits across many files — every edit validates first, and if any fails nothing is written (`dry_run` validates without writing)                                                                                                                       |
| `bash`                                                                                                                                   | Run a shell command (timeout kill; `trace: true` echoes each line) — **registered by default**, withheld with `"tools": {"bash": "off"}` in settings (any scope)                                                                                                                                 |
| `grep` · `glob`                                                                                                                          | Regex content search (ripgrep) · glob file discovery (fd)                                                                                                                                                                                                                                        |
| `graph_query`                                                                                                                            | Query the indexed code graph: symbol definitions/references, file imports/importers/neighborhood — auto-built at session start, refreshed live                                                                                                                                                   |
| `read_symbol`                                                                                                                            | Read a named symbol's exact source span, resolved through the code graph — no line-offset guessing; multiple definitions are listed, never silently picked                                                                                                                                       |
| `build_project` · `run_tests`                                                                                                            | Build/test with the workspace's toolchain (cargo/npm/go/make)                                                                                                                                                                                                                                    |
| `diagnostics`                                                                                                                            | Fast typecheck: the toolchain's native machine-readable check (`cargo check` / `tsc` / `eslint` / `ruff`) parsed into structured file:line:col records, grouped by file                                                                                                                          |
| `run_lint` · `format_code`                                                                                                               | The project's own linter/formatter (cargo clippy/fmt, or package.json `lint`/`format` scripts), spawned argv-style — no shell                                                                                                                                                                    |
| `run_script`                                                                                                                             | Run a verb the project itself declares (Makefile target, package.json script, cargo alias); unknown names list the discovered vocabulary                                                                                                                                                         |
| `start_process` · `read_output` · `send_stdin` · `stop_process`                                                                          | Long-running processes (dev servers, REPLs, watchers) from an argv vector — capped output ring, SIGTERM-then-kill stop, reaped at session end                                                                                                                                                    |
| `repo_status` · `repo_diff` · `repo_commit` · `repo_push` · `repo_pull` · `repo_rollback`                                                | Vendor-neutral repository tools: structured status, hunk-level pending-change diffs for pre-commit self-review, pathspec-explicit commits, pushes that structurally refuse the default branch (never forced), fast-forward-only pulls, restore-named-paths rollback                              |
| `verify_done`                                                                                                                            | Replay new test files against `git HEAD` to prove the change works                                                                                                                                                                                                                               |
| `project_overview` · `gather_context`                                                                                                    | Orient in the workspace in one pass · one deterministic context sweep (greps, globs, symbol lookups, bounded excerpts) saved as a reusable pack                                                                                                                                                  |
| `explorations` · `save_exploration`                                                                                                      | Shared codebase maps — explore once, reuse everywhere                                                                                                                                                                                                                                            |
| `save_memory` · `cite_memory`                                                                                                            | Persist a lesson into every future session's system prompt · cite a recalled memory so it earns its place                                                                                                                                                                                        |
| `task_create` · `task_list` · `task_start` · `task_complete` · `task_cancel` · `task_assign`                                             | The session task board — one row per deliverable, exactly one in progress, `task_assign` delegates to a parallel sub-agent                                                                                                                                                                       |
| `search_skills` · `install_skill` · `skill_search` · `tool_search` · `mcp_search`                                                        | Discovery at the session layer: search the public skills registry and install from it (with confirmation), search the skills already installed, or rank this session's tools / MCP servers instead of carrying all of them in the prompt                                                         |
| `ci_status`                                                                                                                              | CI runs + failure logs via `gh`                                                                                                                                                                                                                                                                  |
| `screenshot`                                                                                                                             | Capture the screen as verification evidence                                                                                                                                                                                                                                                      |
| `web_fetch` · `web_extract_assets` · `web_download` · `web_search`                                                                       | Read a URL as markdown/text/HTML · mine a page's stylesheets, scripts, and design tokens · download an asset into the workspace · ranked search results — **registered by default**, withheld with `"tools": {"web": "off"}` in settings (any scope); `web_search` additionally needs your own `BRAVE_API_KEY` or `TAVILY_API_KEY` |
| `generate_svg`                                                                                                                           | Validate, sanitize, and save an agent-authored SVG under `.stella/artifacts/` — scripts, handlers, and external references stripped                                                                                                                                                              |
| `generate_image` · `generate_video` · `poll_video`                                                                                       | Text-to-image/video via your provider key, saved under `.stella/artifacts/` — registered only when a media-capable key is set (video is behind a cost confirmation)                                                                                                                              |
| `ask_user`                                                                                                                               | Put a 2–6 option multiple-choice question to you when the decision is genuinely yours; a headless run gets a named error instead of a hang                                                                                                                                                       |
| `create_issue` · `update_issue` · `close_issue` · `search_issues` · `get_issue` · `list_labels` · `list_members` · `start_work_on_issue` | Issue tracking (GitHub/Linear) — registered only when a tracker is connected (`stella connect github\|linear`, `LINEAR_API_KEY`, or `gh` auth)                                                                                                                                                   |

All file tools are workspace-root-pinned, and every read/write/edit/delete is
recorded in the Files-Touched ledger (shown per turn as `[C·R·U·D] path`, also
via `/files`).

**Bash ships on, and switching it off bounds the shell tool rather than every
path to a shell.** `bash` is registered like every other built-in; withhold it
per user, org, or project by adding `"tools": {"bash": "off"}` to the
corresponding `settings.json` scope (normal per-field merge — project wins).
Prefer the enumerable-argv tools regardless (build/test/lint/format,
`run_script`'s project-declared verbs, the process group, the `repo_*` tools) —
they never interpret a shell string. Note that `"bash": "off"` removes the
free-form shell _tool_, not the shell _capability_: `build_project` and
`run_tests` take a `command` override, `verify_done` a `test_cmd`, and
`run_script` composes from the scripts index, so all four still reach `bash -c`
behind the `command.started` policy fence.

Stated precisely, because a security claim that overreaches is worse than none:
turning `bash` off removes the tool, not every route to a shell.
`start_process` stays registered by default and takes an argv vector whose
`argv[0]` may itself be an interpreter (`["bash", "-c", …]`). That is why every
model-authored command line — `bash`, `start_process`'s joined argv,
`build_project`/`run_tests`/`verify_done`/`run_script`'s resolved commands —
rides the same blocking `command.started` policy chain, so a hook on that event
sees the exact line _before_ anything spawns
(`stella-tools/src/registry.rs::command_line_for`). Note the scope: the
`guard-deny-command` workspace rule globs the `bash` tool's own `command`
string, not `start_process`'s argv. If you need a boundary rather than a gate,
use the OS sandbox below.

**Opt-in bash sandbox:** `STELLA_BASH_SANDBOX=workspace-write` confines `bash`
file writes to the workspace root plus the standard tmp dirs (network still
allowed); `restricted` additionally denies all network. Backends:
`sandbox-exec` (Seatbelt) on macOS, `bwrap` (bubblewrap) on Linux. This bounds
the blast radius of prompt injection — instructions hidden in a file the agent
reads can steer the model into running arbitrary commands. The tradeoff is
capability: the sandbox also blocks legitimate work (`cargo` writing
`~/.cargo`, `npm`/`pip` caches under `$HOME`, `git push` under `restricted`),
which is why the default is `off`. Fail-closed: an unknown value, a missing
`bwrap`, or an unsupported platform fails the tool call rather than silently
running unsandboxed.

**Conditional tools:** issue tools need `LINEAR_API_KEY` or a `gh auth login`;
`generate_image` needs `ZAI_API_KEY` or `OPENAI_API_KEY`. Without their
prerequisites, these tools are not registered. `graph_query` is **not**
conditional despite needing an index — it builds one on first use, and gating
it on the index existing would hide exactly the tool meant to create it.

**The web tools ship on, and switch off as a family.** `web_fetch`,
`web_extract_assets`, and `web_download` are registered by default like every
other built-in; withhold all three with `"tools": {"web": "off"}` in any
`settings.json` scope. `web_search` additionally needs your own
`BRAVE_API_KEY` or `TAVILY_API_KEY` — no key, no tool. They are the only
built-ins that talk to a host other than your model provider — a fetched page
is untrusted input and an egress channel — which is why the family off-switch
exists.

**Where they may fetch is bounded by default.** Loopback, private ranges,
link-local (the cloud metadata endpoints), and the `localhost` / `.internal` /
`.local` name families are refused — checked on the URL, on the addresses DNS
returns for it, and on every redirect hop, because a page the agent reads can
try to steer it at an internal service. Re-open a specific destination in
`~/.stella/web_auth.toml`:

```toml
[egress]
allow = ["localhost:3000", "127.0.0.1:3000"]   # or ["*"] to switch the guard off
```

See [the web tools](https://stella.oxagen.sh/docs/agent-tools#web-opt-in).

## Memory and context

Lessons saved with `save_memory` (or written as markdown in
`.stella/memories/`) load once at session start into a byte-stable system
prompt, so every model call considers them at prompt-cache prices. New memories
take effect the next session — hot-injection would invalidate the cache.

Every working turn is also recorded as an **episode** (summary, files touched,
outcome, time window) in `.stella/private/context.db`, and `stella init` writes the
domain taxonomy as bi-temporal facts. Recall fans out through the Context Graph
Protocol host to
the memory store and the code graph, fused by score under one budget.

## Telemetry

Executions are recorded, best-effort, in `.stella/private/store.db`: the full event
stream, per-model-call telemetry (tokens, cache hits, cost), and the
Files-Touched ledger. The store is never a dependency of a turn — a session
runs even if the file can't be opened. Query it with any SQLite client.

A default install constructs no telemetry spool and no telemetry HTTP client, so
nothing is sent anywhere. Two explicit configurations, and only these two, change
that: Enterprise enrollment (below) and the `cloud.json` drain
([`stella cloud sync`](https://stella.oxagen.sh/docs/commands/cloud), a separate
pipe with its own wire contract
and endpoint, inert unless the file carries both an `org_id` and a `drain` block).

A seat becomes enrolled only through a valid signed
`enterprise_telemetry` document in the org-managed settings scope. That
document binds issuer, audience, organization/workspace, expiry, the single
`execution_rollup` event class, a managed model catalog, `process_free`
isolation, bearer-secret references, and one endpoint that must exactly match
the administrator's credential-free HTTPS allowlist.

While enrolled, only `stella run --no-pipeline` is eligible. Stella rejects
pipeline, goal, fleet, deck/chat, interactive, workspace-port, and candidate
workspace execution paths because they cannot prove the process-free boundary.
Eligible finalized runs may export only managed organization/workspace/enrollment
identifiers; allowlisted provider/model or `other`; outcome; duration; input and
output token counts; cost in micro-USD; tool-call and changed-file counts; and a
produced-output boolean. Prompts, paths, tool names/arguments/results, reasoning,
errors, git state, memories, rules, full local events, and local execution or
installation identifiers are excluded.

Delivery is at-least-once from an owner-only host spool outside the workspace.
Retained event payloads are bounded to 10,000 rows and 16 MiB; SQLite overhead
may make the physical database larger. Startup flush is detached and never
delays execution or process exit; `stella telemetry flush` attempts one bounded
batch explicitly. `stella telemetry status` reports enrolled/disabled state,
pending and stranded rows/bytes, quarantine and physical size, and durable drop,
corruption, and rollover counters. See the
[Telemetry documentation](https://stella.oxagen.sh/docs/telemetry) for backfill,
retry, rollover, and server-side companion requirements.

## Lifecycle hooks

Declare shell-command hooks in any `settings.json` scope; they fire on agent
lifecycle events, receiving the event payload as JSON on stdin:

```jsonc
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "command": "echo \"on-call: $(cat .oncall 2>/dev/null)\"" },
        ],
      },
    ],
    "PreToolUse": [
      {
        "matcher": "bash",
        "hooks": [{ "command": "./scripts/guard-bash.sh", "timeoutMs": 5000 }],
      },
    ],
  },
}
```

- **`SessionStart`** — stdout is appended to the system prompt as session
  context (once per session).
- **`PreToolUse`** — a non-zero exit blocks the tool; the model sees the hook's
  message instead. `matcher` is a glob over the tool name.
- **`PostToolUse`** — observation only, never blocks.

Scopes concatenate (any scope can add a gate; none can remove another's). Hooks
from a repo's own `.stella/settings.json` load only with
`STELLA_PROJECT_HOOKS=1`, so cloning an untrusted repo never auto-executes its
commands.

## Architecture

`stella-core` has no I/O of its own: it drives model calls through the
`Provider` port and tools through the `ToolExecutor` port, emitting an
`AgentEvent` stream over a channel. All decision logic — compaction, eviction,
loop detection, budget — is plain synchronous functions over owned data, so a
new vendor or tool is an adapter, never a rewrite.

```mermaid
flowchart TD
    U(["stella · the CLI (stella-cli)<br/>REPL · run · goal · monitor"]) --> CORE
    subgraph CORE["stella-core · the engine (NO I/O)"]
      ENG["step driver · goal loop · budget<br/>retry · compaction · loop-detection · router"]
    end
    CORE -->|Provider port| MODEL["stella-model — adapters<br/>anthropic · openai · gemini · vertex · bedrock · zai<br/>(+ any OpenAI-compatible: xai · deepseek · openrouter · local)"]
    CORE -->|ToolExecutor port| TOOLS["stella-tools<br/>CRUD · grep · glob · build · test · lint · scripts · processes · repo · verify_done · issues · CI · bash"]
    MCP["stella-mcp<br/>external MCP servers"] -.->|merges tools into registry| TOOLS
    CORE -->|emits AgentEvent stream| STORE["stella-store<br/>SQLite: executions · events · telemetry"]
    U -->|"recall · episodes · bi-temporal facts"| CTX["stella-context — context plane<br/>recall · embeddings · memory"]
    GRAPH["stella-graph — tree-sitter code index"] -->|"auto-indexed at session start · queried via `graph_query` + `stella graph`"| DB[("SQLite code graph<br/>.stella/private/codegraph.db")]
    MODEL -.->|versioned serde| PROTO["stella-protocol — shared types + Provider/tool ports"]
    TOOLS -.-> PROTO
    STORE -.-> PROTO
```

## Design principles

Eight architectural invariants hold the design together: the engine drives
everything through ports and does no I/O, every cross-boundary type round-trips
through `serde_json` byte-for-byte, errors are typed rather than panicked, the
budget aborts only between steps and never mid-tool, prompts stay byte-stable so
the provider cache keeps hitting, provider feature parity is declared and
witness-tested rather than assumed — and Stella sends **zero telemetry
anywhere** by default.

They are stated normatively, in full, in
[AGENTS.md § Architecture: ports, not concretions](AGENTS.md#architecture-ports-not-concretions).
That is the only copy: this section is a summary and does not govern. A PR that
breaks one of them will be asked to restructure regardless of how good the
feature is.

Stella is also **BYOK** — any provider key, any combination, no account. That is
a product property rather than an architectural invariant, but it is the one
most people want to know first.

## Workspace layout

Nineteen `stella-*` crates make up the workspace. The Context Graph Protocol
(CGP) —
the retrieval abstraction Stella's recall routes through — now lives in its own
repository and is pulled in as registry crates pinned to exact versions in the
root `[workspace.dependencies]`, not as workspace members.

Every crate carries its own `README.md` — linked from the table below — with its
file layout, the invariants it enforces, its gotchas, and the recipe for
extending it.

| Crate                                                | Role                                                                                                                                                                                                                                   |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`stella-cli`](stella-cli/README.md)                 | CLI binary — clap surface + agent loop wiring                                                                                                                                                                                          |
| [`stella-core`](stella-core/README.md)               | The step-driver engine (no I/O): parallel tools, goal loop, budget, retry, compaction, loop detection, router                                                                                                                          |
| [`stella-tools`](stella-tools/README.md)             | The built-in tools (CRUD, `grep`/`glob`, build/test/lint/format, `run_script`, the process group, the `repo_*` tools, `verify_done`, issues, CI, `bash` — every one registered by default, each withholdable via `tools` switches)                                                              |
| [`stella-model`](stella-model/README.md)             | The `Provider` port's adapters: anthropic, openai, gemini, vertex, bedrock, zai (SSE, tool-call dialects, SigV4, pricing)                                                                                                              |
| [`stella-store`](stella-store/README.md)             | SQLite persistence — executions, events, telemetry, files-touched                                                                                                                                                                      |
| [`stella-mcp`](stella-mcp/README.md)                 | MCP client (stdio + HTTP, protocol `2025-06-18`) merging external tools into the registry                                                                                                                                              |
| [`stella-protocol`](stella-protocol/README.md)       | Zero-logic, zero-I/O stability contract: shared serde types + the `Provider`/tool ports                                                                                                                                                |
| [`stella-context`](stella-context/README.md)         | The context plane: reflection-memory recall + embedding index, episodes, bi-temporal facts                                                                                                                                             |
| [`stella-graph`](stella-graph/README.md)             | Tree-sitter symbol + import-edge indexer (Rust/Python/JS/TS/TSX/SQL/Go/Java/C/PHP)                                                                                                                                                     |
| [`stella-pipeline`](stella-pipeline/README.md)       | The orchestration plane above the engine — the default `stella run` path: triage → plan → scope review → witness → execute → verify → judge ([docs](https://stella.oxagen.sh/docs/inference-pipeline))                                 |
| [`stella-fleet`](stella-fleet/README.md)             | The multi-agent fleet behind `stella fleet`: DAG planner + wave scheduling, a shared tree with cooperative file claims by default, opt-in git-worktree isolation per task                                                              |
| [`stella-media`](stella-media/README.md)             | Multimodal generation behind one `MediaProvider` port — `generate_svg` always on; `generate_image` and `generate_video`/`poll_video` registered when a media-capable key is set (video behind a headless cost gate)                    |
| [`stella-tui`](stella-tui/README.md)                 | The Command Deck — a pure event-fold core + thin crossterm shell                                                                                                                                                                       |
| [`stella-observatory`](stella-observatory/README.md) | The Observatory — `stella observe`'s loopback-only telemetry dashboard over the local SQLite stores                                                                                                                                    |
| [`stella-serve`](stella-serve/README.md)             | A separate headless binary (not part of the `stella` CLI): drives the engine over a wire protocol so a host process runs the Rust core, remoting every model and tool call back — the engine holds no ambient authority                |
| `stella-diag`                                        | The diagnostics plane: typed, content-free records explaining *why* the program did something — a `serde`-only leaf every crate may depend on                                                                                          |
| `stella-engine`                                      | Step-scoped facade over `stella-core` for durable hosts: `run_step` + checkpoint/resume, re-exports only — consumed by `stella-serve`, never linked by the CLI                                                                          |
| `stella-runtime`                                     | The shared engine-assembly bottom half (`RuntimeSpec` → `SessionRuntime`): provider, registry, store, budget — construction only, and it reads no ambient environment by contract                                                       |
| `stella-parity`                                      | The CLI-vs-API capability matrix: every engine capability declares a witnessed posture on both surfaces, so a feature cannot ship on one and silently miss the other                                                                    |
| Context Graph Protocol                               | Its own project now: [macanderson/context-graph-protocol](https://github.com/macanderson/context-graph-protocol) — wire types, host runtime, and the public conformance suite. Stella is its reference host and depends on it as exact-version registry crates. |

Alongside the Rust workspace, the documentation site
([stella.oxagen.sh](https://stella.oxagen.sh)) lives at `website/` (Next.js +
Fumadocs) as a **self-contained** package: its own `package.json`,
`pnpm-lock.yaml`, and pnpm settings all sit in that directory, and the repo
root is pure cargo. The two toolchains share no code — the only thing that
crosses between them is the brand palette: `stella-tui/src/palette.rs` is the
hand-maintained normative source, mirrored by `website/src/app/tokens.css`
(`--stella-*`), and the two must be edited together.

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p stella-cli -- models
```

### The docs site

```bash
cd website       # the site is self-contained; the repo root is pure cargo
pnpm install     # once (Node ≥ 20, pnpm 11)
pnpm dev         # serve the docs at http://localhost:3400
pnpm build       # production build (what docs.yml CI runs)
```

Docs content is MDX under `website/content/docs/`. On a pull request a
docs-only change runs the fast `docs` workflow instead of the Rust gate; the
merge queue does not honor `paths-ignore`, so it still pays the full gate once
queued — deliberately, since the required check has to report on the merged
result.

To try your working copy against real projects before a release, install it as
`stella-dev` — it lives side by side with the released `stella`:

```bash
scripts/dev.sh install        # build (release) + link ~/.local/bin/stella-dev
cd ~/any/other/repo
stella-dev                    # the Command Deck, running your checkout
scripts/dev.sh status         # show what both binaries resolve to
scripts/dev.sh uninstall      # remove the link
```

## Contributing

Contributions are welcome. Stella is AGPL-3.0-only and dual-licensed, so a
one-time [CLA](CLA.md) signature is required — you keep your copyright, and the
bot walks you through it on your first PR. See [`CONTRIBUTING.md`](CONTRIBUTING.md)
for dev setup, a tour of the crates, the witness-test contract, and style rules.
CI runs `fmt`, `clippy -D warnings`, tests, and a release build on every PR.

| You have…  | Do this                                                                                                                                                                            |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A bug      | [File it with a repro](https://github.com/macanderson/stella/issues/new?template=bug_report.yml)                                                                                   |
| An idea    | [Open a feature request](https://github.com/macanderson/stella/issues/new?template=feature_request.yml) or start a [discussion](https://github.com/macanderson/stella/discussions) |
| An evening | Grab a [`good first issue`](https://github.com/macanderson/stella/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)                                                    |

## License

Stella is **dual-licensed**.

**Open source: [AGPL-3.0-only](LICENSE).** Free to run, read, modify, and
redistribute. In exchange, if you distribute a modified Stella — or offer one to
users over a network — you publish your modifications under the same terms.
Using Stella as a coding tool on your own proprietary codebase is unaffected:
the AGPL covers Stella itself, not the code you write with it.

**Commercial: [available from Oxagen](LICENSING.md).** If you want to embed
Stella in a closed-source product, run a modified Stella as a hosted service
without publishing it, or your procurement process forbids AGPL code, a
commercial license removes those obligations. Contact <licensing@oxagen.sh>.

[`LICENSING.md`](LICENSING.md) explains which track you are on and why. The
[Context Graph Protocol](https://github.com/macanderson/context-graph-protocol)
is a separate project and stays permissive — **MIT OR Apache-2.0**, at your
option — so depending on it does not put your project under the AGPL.

Contributions require a [CLA](CLA.md); you keep your copyright.
