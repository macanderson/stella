# Stella Harbor Adapter

Harbor agent adapter for running the Stella coding CLI on SWE-bench and Terminal-Bench.

## What this does

This adapter integrates Stella with the [Harbor framework](https://www.harborframework.com/), enabling head-to-head benchmarking against other coding agents (Claude Code, Codex CLI, Oxagen, Aider, etc.) on the same datasets with the same verifier.

## Quick start

```bash
cd bench/harbor_adapter

# Install the adapter package
pip install -e .

# Run SWE-bench Verified (default model: anthropic/claude-fable-5)
export ANTHROPIC_API_KEY=...
STELLA_MODEL=anthropic/claude-fable-5 ../../oxagen-platform/bench/swe-bench/run.sh

# Or run directly with Harbor (after installing Harbor)
harbor run \
  --agent stella \
  --dataset "swe-bench/swe-bench-verified" \
  --n-concurrent 4 \
  --env ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY
```

## Configuration

### Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `STELLA_MODEL` | `anthropic/claude-fable-5` | Model to use (provider/model_id) |
| `STELLA_BUDGET` | `5.0` | Per-task USD spend limit |
| `STELLA_BINARY` | auto | Path to Stella binary (auto-detects `stella` on PATH or `./target/release/stella`) |
| `STELLA_TIMEOUT` | `1800` | Per-task timeout in seconds |
| `STELLA_BASE_URL` | provider default | Override API base URL (e.g., for Z.ai coding endpoint) |

### Provider API keys

Stella uses BYOK (bring-your-own-key). Export the key for your chosen provider:

```bash
export ANTHROPIC_API_KEY=...   # for anthropic/* models
export ZAI_API_KEY=...         # for zai/* models
export OPENAI_API_KEY=...       # for openai/* models
export GEMINI_API_KEY=...      # for gemini/* models
```

### Z.ai (GLM) Configuration

For Z.ai GLM models, use the coding-specific base URL:

```bash
export ZAI_API_KEY=...
export STELLA_BASE_URL=https://api.z.ai/api/coding/paas/v4
STELLA_MODEL=zai/glm-5.2
```

**Important**: The base URL must include `/coding/` for Z.ai coding plans. Use `https://api.z.ai/api/coding/paas/v4`, not `https://api.z.ai/api/paas/v4`.

## Building Stella

Build the release binary before running benchmarks:

```bash
# From the stella-cli repo root
cargo build --release -p stella-cli

# The binary will be at ./target/release/stella
```

## Adapter internals

The adapter:
1. Locates the Stella binary (`STELLA_BINARY`, PATH, or `./target/release/stella`)
2. Uploads it to the Harbor container as `/usr/local/bin/stella`
3. Provisions fast-search tools (rg, fd) if available on the host
4. Runs Stella one-shot: `stella --model <model> --budget <usd> run "<instruction>"`
5. Captures logs and extracts cost/token/step metadata

## License

MIT OR Apache-2.0
