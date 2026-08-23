<p align="center">
  <picture>
    <source srcset="docs/brand/logo/svg/lockup-color-light.svg">
    <img src="docs/brand/logo/svg/lockup-color-light.svg" alt="stella" width="300">
  </picture>
</p>

<p align="center"><strong>Reference Grade Agent Loop</strong></p>
<p align="center">Open Source · Rust · BYOK · No Phone Home</p>

<p align="center">
  <a href="https://github.com/macanderson/stella/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/macanderson/stella/ci.yml?branch=main&style=flat-square&logo=github&label=ci" alt="CI status"></a>
  <a href="https://github.com/macanderson/stella/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/macanderson/stella/release.yml?style=flat-square&logo=github&label=release" alt="Release status"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-0A0A0C?style=flat-square" alt="License"></a>
  <img src="https://img.shields.io/badge/rust-1.90%2B-0A0A0C?style=flat-square&logo=rust&logoColor=EFC53F" alt="Rust 1.90+">
  <img src="https://img.shields.io/badge/providers-9%20%2B%20local-0A0A0C?style=flat-square" alt="9 providers + local">
</p>

<p align="center">
  <a href="https://stella.oxagen.sh"><b>Website</b></a> ·
  <a href="https://stella.oxagen.sh/docs"><b>Docs</b></a> ·
  <a href="https://stella.oxagen.sh/docs/getting-started/installation"><b>Quickstart</b></a>
</p>

Stella is an open-source, bring-your-own-key (BYOK) coding agent that runs in
your terminal. It supports nine hosted model providers plus any local
OpenAI-compatible server, keeps canonical telemetry in a local SQLite
database, and enforces a hard per-run budget. Telemetry leaves your machine
only through two explicit paths — an enrolled Oxagen Enterprise managed
install, or a `drain` block in `~/.stella/cloud.json` — and neither exists in
a default install. Built in Rust as a workspace of focused crates.

## Features

- **BYOK, auto-detected** — Set one provider's API key and Stella detects it.
  Pin a model per run or shell with `--model`.
- **Deterministic definition of done, opt-in** — `stella run --pipeline
  <plugin-id>` hands the turn to an installed verification plugin, whose
  oracle authors a test that fails on the old code and passes on the new and
  tracks that fail→pass flip itself. A green suite alone is not accepted on
  that path — but the evidence is self-reported: Stella evaluates it against
  the plugin's declared rule and never re-runs it. The built-in staged
  pipeline (`--pipeline classic`) has been deleted (#3865); the flag is
  refused outright, naming `stella plugin install` as the remedy. Oxagen's
  Vera is the reference verification plugin, private and not shipped here.
- **Single-threaded engine** — One deterministic step loop: plan, fan tools
  out in parallel, observe, compact if noisy, repeat. No coordinator or
  multi-agent swarm.
- **Prompt-cache-native memory** — Lessons in `.stella/memories/` load once at
  session start into a byte-stable system prompt (~0.1× input cost).
- **Code graph** — A tree-sitter symbol/import index (Rust, TS/TSX/JS, Python,
  Go, Java, C, PHP, SQL) queried by `stella search` instead of grepping.
- **Local-first telemetry** — Executions, events, token/cost telemetry, and
  the files-touched ledger stay canonical in `.stella/private/store.db`.
  Community/default sends none of it anywhere. Only enrolled Oxagen Enterprise
  managed mode can derive a closed, content-free operational rollup.
- **Budget enforcement** — `--spend-limit` aborts cleanly between steps, never
  mid-tool.
- **Goal & fleet modes** — `goal` works in judged rounds; `fleet` fans a task
  DAG out to parallel workers that share one tree under cooperative file
  claims, or take their own git worktree when a task opts in.
- **Lifecycle hooks** — Shell-command hooks (`SessionStart`, `PreToolUse`,
  `PostToolUse`) configurable in `settings.json`.

## Prerequisites

- **macOS or Linux**, `x86_64` or `arm64`. Private persistence depends on Unix
  owner/mode and no-follow primitives; non-Unix builds fail closed for
  sensitive state writes, and Windows persistence is not supported.
- For prebuilt / Homebrew install: `curl`.
- For building from source: **Rust 1.90+** (via [rustup](https://rustup.rs))
  and `git`. A clone of this repository uses the toolchain pinned in
  `rust-toolchain.toml` (currently 1.97.0); rustup downloads it automatically
  on the first `cargo build`.
- An API key for any supported provider, _or_ a local OpenAI-compatible model
  server (Ollama, vLLM, LM Studio, llama.cpp).

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

The `stella-*` crates are **not published to crates.io** — `publish = false`
is set once at `[workspace.package]` in the root `Cargo.toml` and inherited by
every member. The `cargo install --git …` command above is the only supported
cargo path; dropping `--git` does **not** install this project.

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
| **Amazon Bedrock**     | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (+ `AWS_REGION`) | `us.anthropic.claude-sonnet-4-5-20250929-v1:0`                  |
| **Local**              | _none_ — pass `--base-url`                    | whatever your server hosts                                                       |

OpenRouter is checked first — its key is gateway-specific, so having one is a
deliberate choice — and it brings a whole default posture, not just a default
model: Kimi K3 driving at `xhigh` with thinking on, `anthropic/claude-opus-5`
judging, `z-ai/glm-5.2` triaging, all on the one key. Every field composes
underneath your own settings, so anything you configure wins. OpenRouter slugs
keep their vendor namespace on the wire, so pinning one on the CLI needs both
halves: `--model openrouter/moonshotai/kimi-k3`.

```bash
export ANTHROPIC_API_KEY=your_key_here     # or OPENAI_API_KEY, GEMINI_API_KEY, …
```

Bedrock is the one provider that authenticates with more than one value —
SigV4 needs an access key id *and* a secret access key, plus a session token
for temporary credentials, and a region. `stella auth set` stores the whole
set:

```bash
stella auth set bedrock                    # prompts for each, secrets masked
stella auth set bedrock --stdin \
  --field AWS_SECRET_ACCESS_KEY="$SECRET" \
  --field AWS_REGION=eu-central-1          # scripted equivalent
```

Explicit credentials only: AWS profile files, SSO caches, IMDS/container
roles, and web-identity token files are deliberately not consulted, so a
Stella process never authenticates as whatever identity its host happens to be
carrying. Bedrock is also checked **last** during auto-detection, and only
when a secret access key resolves too — `AWS_ACCESS_KEY_ID` is exported in
plenty of shells for reasons that have nothing to do with Bedrock.
`--model bedrock/…` pins it regardless.

Pin a provider/model per run or shell:

```bash
stella --model anthropic/claude-fable-5 run "refactor the database layer"
export STELLA_MODEL=openai/gpt-5.5
```

**Local / any OpenAI-compatible gateway** — no key required:

```bash
stella --model local/llama3.3 --base-url http://localhost:11434/v1 chat
```

**Z.ai GLM Coding Plan:** set `ZAI_GLM_CODING_PLAN=1` alongside `ZAI_API_KEY`
to route through the dedicated coding endpoint.

**Credential chain** (first hit wins): `--api-key` flag → provider env var →
`settings.json` `api_key` → `~/.stella/credentials.toml` → interactive prompt.

`credentials.toml` is written by
[`stella auth`](https://stella.oxagen.sh/docs/commands/auth) — `auth set
<provider>` stores a key (prompted and masked unless you pass
`--key`/`--stdin`), `auth list` shows every stored key redacted alongside the
source that actually wins, and `auth remove <provider>` deletes one. It never
prints a secret value.

**Project `.env` files** — Stella reads `.env`, `.env.local`, and
`.env.<mode>.local` (e.g. `.env.production.local`) from the working directory
(or the nearest ancestor within the same git repo) into the environment at
startup, most-specific file first. Template files (`.env.example`,
`.env.sample`, `.env.dist`) and non-`.local` mode files (`.env.production`)
are never read. **Your live shell always wins** — an exported value is never
overwritten by a file, so unset a stale export if you mean to switch. Disable
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
> (`base_url`, `api_key`, `api_key_env`, and `mcp.registry_url`) are
> **ignored** unless you opt in with `STELLA_TRUST_PROJECT=1`, so a hostile
> repo can't silently point your real API key at its own server. Cosmetic
> fields (`name`, `default_model`, `dialect`) still apply; the user and
> org-managed scopes are always trusted. Project hooks are gated the same way,
> via `STELLA_PROJECT_HOOKS`, and so are **project-scope plugins**
> (`<workspace>/.stella/plugins`): a plugin declares a program Stella spawns
> and can arbitrate the agent loop, so one that arrived with a `git clone` is
> not loaded, listed, or dispatched until you set `STELLA_TRUST_PROJECT=1`.
> Plugins you installed yourself with `stella plugin install --scope user`
> live in `~/.stella/plugins` and are unaffected.

### Agent engine config (`agent_engine_config`)

The engine runs a configurable agent per role — **default** (the interactive /
step-loop agent) and the pipeline's **worker**, **verifier**, **triage**,
**research**, and **plan**. The `agent_engine_config` object in the same
`settings.json` scope chain configures each one's model, gateway, system
prompt, reasoning, and sampling parameters. In the Command Deck, `/settings`
opens the SETTINGS tab, whose engine-config editor covers all of it (`s` saves
to user scope, `S` to project scope; the per-agent model pickers offer
`allowed_models`, falling back to the catalog when that list is empty). There
are no per-agent slash commands — the SETTINGS tab is the one place models are
configured.

```jsonc
{
  "agent_engine_config": {
    // The session's model ("provider/slug", or a bare catalog slug).
    "default_model": "anthropic/claude-fable-5",

    // A model for a role an installed plugin declared, keyed
    // "<plugin-id>/<role>". Unset seats run on the session's model.
    "seat_models": {"vera/verifier": "openrouter/openai/gpt-5.5"},

    // The model vocabulary the TUI pickers offer and auto_mode selects from.
    "allowed_models": [
      "anthropic/claude-fable-5",
      "zai/glm-5.2",
      "openrouter/openai/gpt-5.5",
    ],

    // "on" = pick the verifier automatically from allowed_models: prefer a
    // different model family than the worker's, then the highest catalog
    // price tier.
    "auto_mode": "off",
    // "on" = per-agent effort is chosen for you (verifier high, worker and
    // plan medium, triage and research low), overriding any per-agent
    // "effort".
    "effort_auto": "off",
    // "on" = thinking mode chosen for you (on everywhere except triage and
    // research, which read rather than deliberate).
    "reasoning_auto": "off",

    // Per-agent deep config. Every field is optional — set it and it goes
    // on the wire; leave it out and the provider default applies.
    "agents": {
      "verifier": {
        "provider": "openrouter", // gateway: the slug goes to THIS
        "model": "openai/gpt-5.5", // provider verbatim (BYOK per agent)
        "prompt": "You are a strict, evidence-first code verifier.",
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
`pipeline_<agent>_model` > `default_model` > auto-detect. Research and plan
end that chain at the **worker** instead of `default_model` — unset, they run
whatever the worker runs, so a `--model` that re-points the worker for one
invocation cannot split them onto a second model. An agent's `provider` field
routes its slug through that gateway verbatim, so the worker can run on your
Anthropic key while the verifier routes `openai/gpt-5.5` through your
OpenRouter key and triage hits Z.ai. Each adapter forwards only the parameters
its wire supports (`verbosity` and `service_tier` are dropped where
meaningless); reasoning maps to GLM's `thinking`, OpenRouter's `reasoning`,
Anthropic extended thinking (with an effort-tiered budget), OpenAI
`reasoning.effort`, and Gemini `thinkingLevel`. Custom prompts replace the
built-in base instructions; workspace memories and rules still append. A seat
model whose provider has no resolvable key degrades softly — that seat rides
the session's model and a notice says so.

## Usage

### Command index

The full subcommand surface. Every command also answers `stella <command> --help`;
each row links to its reference page on [stella.oxagen.sh](https://stella.oxagen.sh/docs/commands).

| Command                                                                     | What it does                                                                                                      |
| --------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| [`run <prompt>`](https://stella.oxagen.sh/docs/commands/run)                | Send a one-shot prompt, non-interactive — the raw step-loop by default; `--pipeline <plugin-id>` opts into an installed verification plugin                          |
| [`chat`](https://stella.oxagen.sh/docs/commands/chat)                       | Interactive session: the Command Deck TUI (also what a bare `stella` opens)                                       |
| [`resume [id]`](https://stella.oxagen.sh/docs/commands/resume)              | Reopen a durable past session exactly where it stood; `--list` browses them                                       |
| [`daemon <cmd>`](https://stella.oxagen.sh/docs/commands/daemon)             | Find, watch, and stop runs that outlived the terminal that started them                                           |
| [`goal <goal>`](https://stella.oxagen.sh/docs/commands/goal)                | Work in judged rounds until a verifier model confirms the goal is met                                             |
| [`monitor [target]`](https://stella.oxagen.sh/docs/commands/monitor)        | Watch a branch/PR's CI and fix failures until it is fully green                                                   |
| [`self-driving <cmd>`](https://stella.oxagen.sh/docs/commands/self-driving) | Drive the perpetual delivery loop: plan a cycle, fold the ledger, advance the audit                               |
| [`fleet <tasks…>`](https://stella.oxagen.sh/docs/commands/fleet)            | Fan tasks out to worker agents, wave-scheduled and recorded in a ledger                                           |
| [`init`](https://stella.oxagen.sh/docs/commands/init)                       | Infer this workspace's domain taxonomy and build the code-graph index                                             |
| [`search <query>`](https://stella.oxagen.sh/docs/commands/search)           | Find code by meaning or by name over the code-graph index                                                         |
| [`storage <cmd>`](https://stella.oxagen.sh/docs/commands/storage)           | Inspect the storage map: layers, namespaces, relations, fields, drift (offline)                                   |
| [`tools`](https://stella.oxagen.sh/docs/commands/tools)                     | List every tool available this session; `--validate` checks custom manifests                                      |
| [`commands <cmd>`](https://stella.oxagen.sh/docs/commands/commands)         | List this workspace's custom slash commands, or convert markdown ones to TOML                                     |
| [`plugin <cmd>`](https://stella.oxagen.sh/docs/commands/plugin)      | Install, list, and remove plugins — `install` shows the declaration before it acts                                       |
| [`models`](https://stella.oxagen.sh/docs/commands/models)                   | List configured providers and available models                                                                    |
| [`auth <cmd>`](https://stella.oxagen.sh/docs/commands/auth)                 | Manage BYOK provider keys in `~/.stella/credentials.toml` — never prints a secret                                 |
| [`config`](https://stella.oxagen.sh/docs/commands/config)                   | Show the fully resolved configuration                                                                             |
| [`migrate <cmd>`](https://stella.oxagen.sh/docs/commands/migrate)           | Move a `settings.json` to `stella.toml`; the JSON is kept, never deleted                                          |
| [`mcp <cmd>`](https://stella.oxagen.sh/docs/commands/mcp)                   | Manage MCP servers: search a registry, install, list, log in, show usage                                          |
| [`memory <cmd>`](https://stella.oxagen.sh/docs/commands/memory)             | Inspect memories; promote one to a project rule                                                                   |
| [`ingest [paths…]`](https://stella.oxagen.sh/docs/commands/ingest)          | Turn markdown you already wrote — `AGENTS.md`, design notes — into steering                                       |
| [`context <cmd>`](https://stella.oxagen.sh/docs/commands/context)           | Review, publish, and explain the context records that steering ingested                                           |
| [`stats`](https://stella.oxagen.sh/docs/commands/stats)                     | Cost, tokens, and $/resolved task for **this** workspace                                                          |
| [`usage <cmd>`](https://stella.oxagen.sh/docs/commands/usage)               | The same numbers across **every** project, from the hub at `~/.stella/usage.db`                                   |
| [`scoreboard`](https://stella.oxagen.sh/docs/commands/scoreboard)           | What the work cost, and whether a merged or closed PR implies anyone called it good                               |
| [`calibration`](https://stella.oxagen.sh/docs/commands/calibration)         | Pass calibration: how often a pass verdict later failed CI, or was reverted                                       |
| [`inspect`](https://stella.oxagen.sh/docs/commands/inspect)                 | Replay the exact context a past model call was sent, verified against its digests                                 |
| [`observe`](https://stella.oxagen.sh/docs/commands/observe)                 | Serve the Observatory dashboard over local telemetry — loopback-only, read-only                                   |
| [`cloud <cmd>`](https://stella.oxagen.sh/docs/commands/cloud)               | Show or set the org/workspace identity that scopes replicated telemetry                                           |
| [`telemetry <cmd>`](https://stella.oxagen.sh/docs/commands/telemetry)       | Inspect or flush the managed enterprise spool — off unless explicitly enrolled                                    |
| [`tune <cmd>`](https://stella.oxagen.sh/docs/commands/tune)                 | A/B one policy knob over two loop-bench result files; `--promote` is reversible                                   |
| [`dataset <cmd>`](https://stella.oxagen.sh/docs/commands/dataset)           | Curate a redacted training dataset from this workspace's own receipts                                             |
| [`arena`](https://stella.oxagen.sh/docs/commands/arena)                     | [arena-bench](https://github.com/macanderson/arena-bench) harness adapter — for benchmarking Stella, not using it |
| [`doctor`](https://stella.oxagen.sh/docs/commands/doctor)                   | Diagnose the install: config, credentials, toolchain, and workspace state                                         |
| [`proposals <cmd>`](https://stella.oxagen.sh/docs/commands/proposals)       | Review the adaptive-context loop's pending proposals — keep, ignore, or retire                                    |
| [`completions <shell>`](https://stella.oxagen.sh/docs/commands/completions) | Print a shell completion script to stdout (bash, zsh, fish, powershell, elvish)                                   |
| [`version`](https://stella.oxagen.sh/docs/commands/version)                 | Print the version and exit                                                                                        |

### Interactive chat (default)

```bash
stella            # or: stella chat
```

On a TTY this opens the **Command Deck** — a tabbed TUI (Session · Agents ·
Traces · Graph · Files · Skills · MCP) with PR-style diffs and an editable
prompt queue. `--accessible` (or `STELLA_ACCESSIBLE=1`) runs that same deck so
a screen reader can read it: inline on your own screen, each finished message
into normal scrollback exactly once, single-column panels, labelled rows
instead of tables, and a spoken line whenever you change tab, overlay, or
focus. `--plain` (or `STELLA_PLAIN=1`, or piped stdio) falls back to the line
REPL.

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
stella fleet --plan .stella/fleet.toml --max-concurrency 2 --spend-limit 5.0
```

Wave-scheduled by dependency and recorded in `.stella/private/fleet.db`.
Workers share the repository root by default, coordinated by cooperative file
claims; a task with `isolation = "isolated"` gets its own git worktree under
`.stella/worktrees/` on a `fleet/<slug>-<hash>` branch instead. A plan file is
the serde form of the fleet DAG: `[[tasks]]` entries with `id`, `title`,
`prompt`, optional `depends_on`, and `isolation`.

### Code graph queries

```bash
stella search "where is run_turn defined"    # a symbol name works as well as a sentence
stella search "what imports src/auth.rs"     # blast radius before you edit it
```

Built by `stella init`, ranked offline by name/graph match when no embedder is
configured, and by meaning when one is.

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

`--model provider/id` · `--api-key` · `--base-url` · `--spend-limit <usd>` ·
`--accessible` · `--plain` · `--no-anim` (also as `STELLA_MODEL`,
`STELLA_BASE_URL`, `STELLA_SPEND_LIMIT`, `STELLA_ACCESSIBLE`, `STELLA_PLAIN`,
`STELLA_NO_ANIM`). All are registered with every subcommand, so they parse
before _or_ after the subcommand token. `--output-format text|json|stream-json`
(env `STELLA_OUTPUT_FORMAT`) is deliberately **not** global: it is declared by
the commands that honor it — `stella run` and `stella fleet` — and goes after
the subcommand token (`stella run "…" --output-format json`); interactive
`chat` / `goal` / `monitor` modes render human-readable output.

`stella run` runs the raw step-loop by default; `--pipeline <plugin-id>` opts
into an installed verification plugin instead. `--pipeline classic` named the
built-in staged pipeline, but that crate is deleted from the workspace (#3865)
and the flag is now refused outright, naming `stella plugin install` as the
remedy; `--no-pipeline` remains a deprecated no-op kept parseable so no script
breaks. A verification plugin's own `[oracle]` decides how the work is proven
([the inference pipeline](https://stella.oxagen.sh/docs/inference-pipeline)
documents the historical design plus the plugin path that replaces it).
`--keep-witness`/`--require-verified` are refused unconditionally now, and
`--test-command` is refused on the raw loop but still passes through to an
installed plugin's own `[oracle]` when one is named with `--pipeline
<plugin-id>` — every refusal names an installed verification plugin as the
remedy.

Post-turn reflection remains enabled for one-shot text, JSON, and stream-JSON
runs. Ephemeral automation can suppress that additional model call with
`STELLA_DISABLE_REFLECTION=1`; the truthy values `true`, `yes`, and `on` are
also accepted case-insensitively.

## Built-in tools

Eighteen built-ins, in six families — the shell, file CRUD, code search, the
task board, sub-agent delegation, scratch state, and the environment probe.
The first three are the working surface; the rest are coordination:

| Tool                                                                                         | Description                                                                                                                                                                                                                             |
| -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bash`                                                                                       | Run a shell command in the workspace root, stdout+stderr with a timeout backstop — the one built-in that runs something nobody bounded, and the only `high`-risk row that is not delegation                                              |
| `read_file` · `write_file` · `edit_file` · `delete_file`                                     | File CRUD confined to the workspace: numbered reads with an offset/limit window, whole-file writes, exact-substring edits that refuse an ambiguous match, and a delete that removes a symlink as the link rather than its target         |
| `search`                                                                                     | Find code by meaning as well as by text — one call returns the answering files with their symbols, callers, imports and source attached, and always states which strategies ran and whether it truncated                                 |
| `task_create` · `task_list` · `task_start` · `task_complete` · `task_cancel` · `task_assign` | The session task board — one row per deliverable, exactly one in progress, `task_assign` delegates a board task to a parallel sub-agent                                                                                                 |
| `delegate`                                                                                   | Hand a self-contained research question to a read-only sub-agent that returns only its findings — bulky evidence stays out of the parent conversation, and independent questions dispatched in one step run concurrently             |
| `save_state` · `get_state` · `list_state` · `delete_state`                                   | The scratch state plane: session-private key/value state (parse results, extracted lists, computed digests) saved once and referenced later instead of re-derived — paged reads by byte offset, deleted automatically at session end     |
| `get_environment`                                                                            | Report the session's environment: workspace root, git status, platform/arch, OS release, shell dialect, and the scratch directory path                                                                                                  |

Every built-in is registered by default and individually withholdable with a
`"tools": {"<name>": "off"}` switch in any `settings.json` scope (normal
per-field merge — project wins). Everything beyond the built-ins reaches the
registry from outside: MCP servers (`.stella/mcp.toml`) and developer-defined
custom script tools (`.stella/tools/*.toml`).

**Containment is the process boundary, not a per-tool sandbox.** The `bash`
built-in runs a shell, and custom manifest tools and hook actions spawn
processes around it — so no per-tool boundary covers them all, and one that
covered only `bash` would claim a session-wide bound it does not have. For
real containment, run the whole Stella process inside a container: that
boundary sits outside every spawn path, so nothing can step around it. See
[`docs/spec/remote-sandboxes.md`](docs/spec/remote-sandboxes.md).

## Memory and context

Lessons written as markdown in `.stella/memories/` load once at session start
into a byte-stable system prompt, so every model call considers them at
prompt-cache prices. New memories take effect the next session — hot-injection
would invalidate the cache.

Every working turn is also recorded as an **episode** (summary, files touched,
outcome, time window) in `.stella/private/context.db`, and `stella init`
writes the domain taxonomy as bi-temporal facts. Recall fans out through the
Context Graph Protocol host to the memory store and the code graph, fused by
score under one budget.

## Telemetry

Executions are recorded, best-effort, in `.stella/private/store.db`: the full
event stream, per-model-call telemetry (tokens, cache hits, cost), and the
Files-Touched ledger. The store is never a dependency of a turn — a session
runs even if the file can't be opened. Query it with any SQLite client.

A default install constructs no telemetry spool and no telemetry HTTP client,
so nothing is sent anywhere. Two explicit configurations, and only these two,
change that: Enterprise enrollment (below) and the `cloud.json` drain
([`stella cloud sync`](https://stella.oxagen.sh/docs/commands/cloud), a
separate pipe with its own wire contract and endpoint, inert unless the file
carries both an `org_id` and a `drain` block).

A seat becomes enrolled only through a valid signed `enterprise_telemetry`
document in the org-managed settings scope. That document binds issuer,
audience, organization/workspace, expiry, the single `execution_rollup` event
class, a managed model catalog, `process_free` isolation, bearer-secret
references, and one endpoint that must exactly match the administrator's
credential-free HTTPS allowlist.

While enrolled, only the default raw `stella run` (no `--pipeline` flag —
`--no-pipeline` is a deprecated no-op and does not change eligibility) is
eligible. Stella rejects `--pipeline classic`, an installed wrapper plugin,
goal, fleet, deck/chat, interactive, workspace-port, and candidate workspace
execution paths because none of them can prove the process-free boundary: a
wrapper, of any kind, spawns a process the boundary is drawn to exclude.
Eligible finalized runs may export only managed
organization/workspace/enrollment identifiers; allowlisted provider/model or
`other`; outcome; duration; input and output token counts; cost in micro-USD;
tool-call and changed-file counts; and a produced-output boolean. Prompts,
paths, tool names/arguments/results, reasoning, errors, git state, memories,
rules, full local events, and local execution or installation identifiers are
excluded.

Delivery is at-least-once from an owner-only host spool outside the workspace.
Retained event payloads are bounded to 10,000 rows and 16 MiB; SQLite overhead
may make the physical database larger. Startup flush is detached and never
delays execution or process exit; `stella telemetry flush` attempts one
bounded batch explicitly. `stella telemetry status` reports enrolled/disabled
state, pending and stranded rows/bytes, quarantine and physical size, and
durable drop, corruption, and rollover counters. See the
[Telemetry documentation](https://stella.oxagen.sh/docs/telemetry) for
backfill, retry, rollover, and server-side companion requirements.

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
        "matcher": "task_assign",
        "hooks": [{ "command": "./scripts/guard-delegation.sh", "timeoutMs": 5000 }],
      },
    ],
  },
}
```

- **`SessionStart`** — stdout is appended to the system prompt as session
  context (once per session).
- **`PreToolUse`** — a non-zero exit blocks the tool; the model sees the
  hook's message instead. `matcher` is a glob over the tool name.
- **`PostToolUse`** — observation only, never blocks.

Scopes concatenate (any scope can add a gate; none can remove another's).
Hooks from a repo's own `.stella/settings.json` load only with
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
    CORE -->|ToolExecutor port| TOOLS["stella-tools<br/>bash · file CRUD · search<br/>task board · sub-agents · scratch state · environment"]
    MCP["stella-mcp<br/>external MCP servers"] -.->|merges tools into registry| TOOLS
    CORE -->|emits AgentEvent stream| STORE["stella-store<br/>SQLite: executions · events · telemetry"]
    U -->|"recall · episodes · bi-temporal facts"| CTX["stella-context — context plane<br/>recall · embeddings · memory"]
    GRAPH["stella-graph — tree-sitter code index"] -->|"auto-indexed at session start · queried via `stella search`"| DB[("SQLite code graph<br/>.stella/private/codegraph.db")]
    MODEL -.->|versioned serde| PROTO["stella-protocol — shared types + Provider/tool ports"]
    TOOLS -.-> PROTO
    STORE -.-> PROTO
```

## Design principles

Eight architectural invariants hold the design together: the engine drives
everything through ports and does no I/O, every cross-boundary type
round-trips through `serde_json` byte-for-byte, errors are typed rather than
panicked, the budget aborts only between steps and never mid-tool, prompts
stay byte-stable so the provider cache keeps hitting, provider feature parity
is declared and witness-tested rather than assumed — and Stella sends **zero
telemetry anywhere** by default.

They are stated normatively, in full, in
[AGENTS.md § Architecture: ports, not direct dependencies](AGENTS.md#architecture-ports-not-direct-dependencies).
That is the only copy: this section is a summary and does not govern. A PR
that breaks one of them will be asked to restructure regardless of how good
the feature is.

Stella is also **BYOK** — any provider key, any combination, no account. That
is a product property rather than an architectural invariant, but it is the
one most people want to know first.

## Workspace layout

Twenty-six `stella-*` crates make up the workspace. The Context Graph Protocol
(CGP) — the retrieval abstraction Stella's recall routes through — lives in
its own repository and is pulled in as registry crates pinned to exact
versions in the root `[workspace.dependencies]`, not as workspace members.

Every crate carries its own `README.md` — linked from the table below — with
its file layout, the invariants it enforces, its gotchas, and the recipe for
extending it.

| Crate                                                | Role                                                                                                                                                                                                                                   |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`stella-cli`](crates/stella-cli/README.md)                 | CLI binary — clap surface + agent loop wiring                                                                                                                                                                                          |
| [`stella-core`](crates/stella-core/README.md)               | The step-driver engine (no I/O): parallel tools, goal loop, budget, retry, compaction, loop detection, router                                                                                                                          |
| [`stella-tools`](crates/stella-tools/README.md)             | The built-in tools (`bash`, file CRUD, `search`, the task board, the `delegate` sub-agent, the scratch state plane, `get_environment` — every one registered by default, each withholdable via `tools` switches)                                |
| [`stella-model`](crates/stella-model/README.md)             | The `Provider` port's adapters: anthropic, openai, gemini, vertex, bedrock, zai (SSE, tool-call dialects, SigV4, pricing)                                                                                                              |
| [`stella-store`](crates/stella-store/README.md)             | SQLite persistence — executions, events, telemetry, files-touched                                                                                                                                                                      |
| [`stella-mcp`](crates/stella-mcp/README.md)                 | MCP client (stdio + HTTP, protocol `2025-06-18`) merging external tools into the registry                                                                                                                                              |
| [`stella-protocol`](crates/stella-protocol/README.md)       | Zero-logic, zero-I/O stability contract: shared serde types + the `Provider`/tool ports                                                                                                                                                |
| [`stella-context`](crates/stella-context/README.md)         | The context plane: reflection-memory recall + embedding index, episodes, bi-temporal facts                                                                                                                                             |
| [`stella-graph`](crates/stella-graph/README.md)             | Tree-sitter symbol + import-edge indexer (Rust/Python/JS/TS/TSX/SQL/Go/Java/C/PHP)                                                                                                                                                     |
| [`stella-fleet`](crates/stella-fleet/README.md)             | The multi-agent fleet behind `stella fleet`: DAG planner + wave scheduling, a shared tree with cooperative file claims by default, opt-in git-worktree isolation per task                                                              |
| [`stella-media`](crates/stella-media/README.md)             | Multimodal generation behind one `MediaProvider` port                                                                                                                                                                                 |
| [`stella-tui`](crates/stella-tui/README.md)                 | The Command Deck — a pure event-fold core + thin crossterm shell                                                                                                                                                                       |
| [`stella-observatory`](crates/stella-observatory/README.md) | The Observatory — `stella observe`'s loopback-only telemetry dashboard over the local SQLite stores                                                                                                                                    |
| [`stella-serve`](crates/stella-serve/README.md)             | A separate headless binary (not part of the `stella` CLI): drives the engine over a wire protocol so a host process runs the Rust core, remoting every model and tool call back — the engine holds no ambient authority                |
| [`stella-diag`](crates/stella-diag/README.md) | The diagnostics plane: typed, content-free records explaining *why* the program did something — a `serde`-only leaf every crate may depend on                                                                                          |
| [`stella-home`](crates/stella-home/README.md)               | Where `~/.stella` is: the user home, the stella home, and the user-tier data dir. A leaf with **no dependencies at all** — the one shape `stella-store` and `stella-observatory` can both depend on, so the resolution stopped being two hand-synced copies |
| [`stella-engine`](crates/stella-engine/README.md) | Step-scoped facade over `stella-core` for durable hosts: `run_step` + checkpoint/resume, re-exports only — consumed by `stella-serve`, never linked by the CLI                                                                          |
| [`stella-runtime`](crates/stella-runtime/README.md) | The shared engine-assembly bottom half (`RuntimeSpec` → `SessionRuntime`): provider, registry, store, budget — construction only, and it reads no ambient environment by contract                                                       |
| [`stella-parity`](crates/stella-parity/README.md) | The CLI-vs-API capability matrix: every engine capability declares a witnessed posture on both surfaces, so a feature cannot ship on one and silently miss the other                                                                    |
| [`stella-autonomy`](crates/stella-autonomy/README.md) | The self-driving loop's decision core: the AIMD controller, aperture ladder, dry-streak oracle, and ledger folds — pure and shared by the CLI and the Observatory so they cannot drift                                                  |
| [`stella-diff`](crates/stella-diff/README.md) | A pure line-oriented unified diff with git's exact hunk shape and no dependencies                                                                                                                                                      |
| [`stella-embed`](crates/stella-embed/README.md) | The embedding seam: the `Embedder` trait, the fingerprint stamped on every stored vector, and cosine ranking                                                                                                                          |
| [`stella-plugin`](crates/stella-plugin/README.md) | Parses and validates a plugin's manifest — what it declares in the turn loop — with no I/O                                                                                                                                             |
| [`stella-transcript`](crates/stella-transcript/README.md) | The shared transcript model plus its two renderers: HTML for the Observatory and a character grid for the TUI                                                                                                                 |
| [`stella-tty`](crates/stella-tty/README.md) | A no-dependency leaf that answers whether a human is around to see and answer a prompt                                                                                                                                                |
| Context Graph Protocol                               | Its own project now: [macanderson/context-graph-protocol](https://github.com/macanderson/context-graph-protocol) — wire types, host runtime, and the public conformance suite. Stella is its reference host and depends on it as exact-version registry crates. |

Alongside the Rust workspace, the documentation site
([stella.oxagen.sh](https://stella.oxagen.sh)) lives at `website/` (Next.js +
Fumadocs) as a **self-contained** package: its own `package.json`,
`pnpm-lock.yaml`, and pnpm settings all sit in that directory, and the repo
root is pure cargo. The two toolchains share no code — the only thing that
crosses between them is the brand palette: `crates/stella-tui/src/palette.rs`
is the hand-maintained normative source, mirrored by
`website/src/app/tokens.css` (`--stella-*`), and the two must be edited
together.

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
docs-only change runs the fast `docs` workflow, and `ci.yml`'s Rust jobs skip
themselves after a cheap diff check — their required contexts still report, as
`skipped`, so the PR can merge (#1892). Once queued or pushed to `main` the
same jobs always run the full gate — deliberately, since the required check
has to report on the merged result.

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
one-time [CLA](CLA.md) signature is required — you keep your copyright, and
the bot walks you through it on your first PR. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for dev setup, a tour of the crates, the
witness-test contract, and style rules. CI runs `fmt`, `clippy -D warnings`,
tests, and a release build on every PR.

| You have…  | Do this                                                                                                                                                                            |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A bug      | [File it with a repro](https://github.com/macanderson/stella/issues/new?template=bug_report.yml)                                                                                   |
| An idea    | [Open a feature request](https://github.com/macanderson/stella/issues/new?template=feature_request.yml) or start a [discussion](https://github.com/macanderson/stella/discussions) |
| An evening | Grab a [`good first issue`](https://github.com/macanderson/stella/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)                                                    |

## License

Stella is **dual-licensed**.

**Open source: [AGPL-3.0-only](LICENSE).** Free to run, read, modify, and
redistribute. In exchange, if you distribute a modified Stella — or offer one
to users over a network — you publish your modifications under the same terms.
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
