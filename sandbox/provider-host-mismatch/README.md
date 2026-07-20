# Provider / host mismatch sandbox

A pre-warmed, zero-dependency sandbox that reproduces the class of failure where
**a perfectly valid API key gets an HTTP 401** — because Stella sent it to the
*wrong host*. Fake keys, a local mock, no real network. Run it with:

```sh
sandbox/provider-host-mismatch/run.sh
```

You need `python3` and a `stella` binary on `PATH` (or `STELLA_BIN=/path/to/stella`).

---

## TL;DR — the bug this was built to explain

Symptom: every prompt fails with
`provider auth error: OpenRouter rejected the credential (HTTP 401 Unauthorized)`,
even though the OpenRouter key is valid.

Root cause: the `stella_dev` launcher is a shell alias that forces `--base-url`
to **Z.ai**:

```sh
alias stella_dev='./target/release/stella --base-url https://api.z.ai/api/coding/paas/v4'
```

`--base-url` is a **global override applied to whichever provider resolves**. With
no `--model`, Stella auto-detects the **OpenRouter** provider (the OpenRouter key
is present and valid) and then sends that OpenRouter key to **Z.ai's** host. Z.ai
returns 401. The error message says "OpenRouter" because Stella labels the error
with the resolved *provider name*, not the host it actually dialed — which is why
the message points at the wrong place.

Proof it's not the key: a read-only probe against real OpenRouter returns **200**:

```sh
curl -s -o /dev/null -w '%{http_code}\n' https://openrouter.ai/api/v1/key \
  -H "Authorization: Bearer $OPENROUTER_API_KEY"   # -> 200, key is fine
```

### The fix

Make the provider and the host agree. Since the alias points at Z.ai, pin the
Z.ai provider (Z.ai uses your `ZAI_API_KEY`, not the OpenRouter key):

```sh
alias stella_dev='./target/release/stella \
  --model zai/glm-4.6 \
  --base-url https://api.z.ai/api/coding/paas/v4'
```

`zai/glm-4.6` and `zai/glm-5.2` (the flagship) are both valid — verified by
completing a real turn on the Z.ai coding endpoint. `stella models list
--provider zai` prints every current slug if you want a different one; pick one
that appears there, since the `run` path (unlike `config`) rejects any slug not
in the catalog.

Confirm before launching the TUI:

```sh
stella --base-url https://api.z.ai/api/coding/paas/v4 --model zai/glm-4.6 config
#   Provider:  Z.ai (GLM ...)      <- matches the host
#   API Key:   e1150a…             <- your ZAI key, not sk-or-…
#   Base URL:  https://api.z.ai/...
```

(If you actually want OpenRouter, just drop the `--base-url` override and run
plain `stella` — provider and host then both default to OpenRouter.)

---

## What the sandbox demonstrates

`run.sh` walks [`scenarios.tsv`](./scenarios.tsv). For each row it:

1. starts [`mock_provider.py`](./mock_provider.py) as a stand-in "host" told which
   single fake key it accepts (that's how it plays "Z.ai" — it 401s anything but
   the Z.ai key);
2. runs **real `stella config`** in a pristine, isolated `HOME` with fake keys in
   the env and the mock as `--base-url`, and reads back which **provider / key /
   base URL** Stella resolved — the deterministic, offline proof of the mismatch;
3. replays the resolved key against the mock over HTTP to observe the host's real
   **200 / 401**;
4. compares to the expected result.

Expected output — all `PASS`:

```
scenario                 | resolved   | host      | expect | verdict
matched-openrouter       | OpenRouter | openrouter | 200    | PASS   <- everything agrees
mismatch-or-to-zai       | OpenRouter | zai       | 401    | PASS   <- THE REPORTED BUG
matched-zai-pinned       | Z.ai (GLM  | zai       | 200    | PASS   <- THE FIX
autodetect-to-zai-host   | OpenRouter | zai       | 401    | PASS   <- no --model, wrong host
trailing-space-key       | OpenRouter | openrouter | 401    | PASS   <- whitespace footgun
quoted-key               | OpenRouter | openrouter | 401    | PASS   <- dotenv-quote footgun
```

The last two rows cover adjacent causes we ruled out for the reported case but
which produce the same 401: the env-var path does **not** trim, so a trailing
space/newline or leftover quotes in a key are sent verbatim. The mock logs the
raw `Authorization` header with `repr()`, so an otherwise-invisible trailing byte
is visible in `run.sh`'s per-scenario mock log.

## Files

| file | purpose |
|------|---------|
| `mock_provider.py` | zero-dep stand-in host; 200 for its one key, 401 for everything else; logs exact auth bytes |
| `scenarios.tsv` | the dataset — one credential/host situation per row |
| `run.sh` | drives real `stella` per scenario and prints a verdict table |

## How to add a scenario

Append a tab-separated row to `scenarios.tsv`:
`name  model_arg  host_key(openrouter|zai)  mangle(none|trailing_space|quoted)  expect(200|401)  note`.
Everything else is automatic.

## Diagnostic recipe (no sandbox needed)

If you hit a provider 401, bisect in ~1 minute:

1. **Is the key actually bad?** `curl .../api/v1/key -H "Authorization: Bearer $KEY"`
   against the *real* provider. 401 → rotate the key; 200 → keep going.
2. **What is Stella resolving?** `stella … config` — check that **Provider**,
   **API Key** head/tail, and **Base URL** all belong to the *same* provider.
   A provider whose name doesn't match the base URL host is this bug.
3. **Run it the way the failing process runs.** A GUI/IDE-launched TUI does not
   source `~/.zshrc`; run `stella config` the same way to see its real env.
