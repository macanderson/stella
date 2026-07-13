# Running the OCP conformance suite

"OCP conformant" means *green on `ocp-conformance`'s suite for your declared
capability set* — a checkable claim, which is what makes third-party
adoption safe. This page covers both ways to run it: the `ocp-inspect` CLI
binary, and calling the suite as a library from your own test harness.

## The five checks

| check | what it proves | fails when |
|---|---|---|
| `handshake` | the provider completes the handshake and reports a non-empty identity + capabilities | the handshake errors, times out, or `name`/`version` is empty |
| `frame-validity` | every returned frame is citable and scored honestly | any frame's `score` is outside `[0, 1]`, or its `title`/`citation_label` is empty |
| `budget-honesty` | the provider never lies about `token_cost` | returned frames' summed `token_cost` exceeds the query's `max_tokens` |
| `shutdown-clean` | the provider tears down without error | `shutdown` errors or the provider vanishes before it can respond |
| `malformed-input-tolerance` | a garbage line on the wire doesn't crash the provider | the provider dies (stdio only — skipped for HTTP/in-process targets) |

A run's overall verdict, `ConformanceReport::passed()`, is true iff **no
check failed** — a skipped check never fails a run (e.g.
`malformed-input-tolerance` is `Skipped`, not `Fail`, for an HTTP or
in-process provider, since that probe is wire-level and stdio-specific).

The suite is deliberately adversarial. Pointed at a provider that lies about
costs, emits an out-of-range score, omits a citation label, or dies
mid-query, the matching check fails loudly with an evidence string that
names the exact violation — never a bare "not conformant."

## Option A: the `ocp-inspect` CLI

Install the binary (it ships inside the `ocp-conformance` crate):

```bash
cargo install ocp-conformance
```

Run it against a stdio provider:

```bash
ocp-inspect stdio -- ./my-provider --some-flag
```

Or a remote HTTP provider:

```bash
ocp-inspect http https://my-provider.example.com/ocp
```

Add `--query "some goal text"` to also fire an interactive test query before
the conformance run, and `--json` to get the report as machine-readable JSON
(handy for CI) instead of colored terminal output.

`ocp-inspect` exits with a non-zero status when the provider is **not**
conformant, so it's directly usable as a CI gate:

```bash
ocp-inspect stdio --json -- ./my-provider > report.json || {
  echo "provider is not OCP conformant"; cat report.json; exit 1;
}
```

Sample colored output for a fully conformant provider:

```
── conformance: stdio: ./my-provider ──
  ✓ handshake
      provider 'my-provider' v0.1.0 — data-flow reads=true writes=false egress=false; query kinds=["doc"], upsert=false, graph=false
  ✓ frame-validity
      2 frame(s) — all scores in [0,1], titles + citation labels present
  ✓ budget-honesty
      2 frame(s) sum to 128 token(s), within the 4096-token budget
  ✓ shutdown-clean
      provider acknowledged shutdown and tore down cleanly
  ✓ malformed-input-tolerance
      provider ignored a malformed line and still answered a valid query
  CONFORMANT — 5 passed, 0 skipped
```

## Option B: as a library, from your own test suite

`run_conformance` is a plain async function that returns a typed
`ConformanceReport` — call it directly from an integration test:

```rust,no_run
use ocp_conformance::{ProviderTarget, run_conformance};

#[tokio::test]
async fn my_provider_is_ocp_conformant() {
    let report = run_conformance(ProviderTarget::Stdio {
        program: env!("CARGO_BIN_EXE_my_provider").to_string(),
        args: vec![],
    })
    .await;

    assert!(
        report.passed(),
        "not conformant: {:?}",
        report.failures().collect::<Vec<_>>()
    );
}
```

`ProviderTarget` has three variants:

- `ProviderTarget::Stdio { program, args }` — spawn a child process (all five
  checks run).
- `ProviderTarget::Http { url }` — POST to a remote endpoint
  (`malformed-input-tolerance` is skipped — it's a stdio wire-level probe).
- `ProviderTarget::InProcess(Box<dyn ContextProvider>)` — an
  already-constructed in-process provider, e.g. a built-in you want to
  regression-test in your own workspace without spawning anything
  (`malformed-input-tolerance` is skipped for the same reason).

`ConformanceReport` gives you `passed()`, `failures()` (an iterator over just
the failed checks), and `tally()` (`(passed, failed, skipped)` counts) for
building your own reporting on top.

## The stella-repo fixture, for reference

`ocp-conformance`'s own test suite
(`ocp-conformance/tests/conformance_suite.rs`) runs the checks against a real
bundled reference provider, `ocp-example-docs`, including a `--misbehave
<mode>` flag that deliberately trips one check at a time (`lying-costs`,
`bad-score`, `empty-citation`, `bad-version`, `crash-on-query`,
`crash-on-garbage`). Reading those tests is the fastest way to see exactly
what evidence string each failure mode produces, and doubles as proof that
the suite genuinely catches a broken provider rather than rubber-stamping
everything.
