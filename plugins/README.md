# `plugins/` — first-party plugins, deliberately outside the workspace

A **plugin** is a directory with a `plugin.toml` manifest and a program. The
manifest declares what say the plugin wants in Stella's turn loop and what it
wants to reach outside it; a human reads that declaration at install and says
yes or no. Nothing a plugin does is inferred.

| Plugin | Replaces | Points | Status |
| --- | --- | --- | --- |
| [`stella-research/`](stella-research/) | the pipeline's research **and recall** stages | `before_turn` | Track B's first extraction (#3380 §7); recall asks the host for the context plane (#3540) |

## Why these are not workspace members

`Cargo.toml`'s `members` list names twenty-five crates and none of them is
here, on purpose. The whole point of `doc:pipeline-as-plugins` is to *uncouple*
the staged pipeline from the binary — its stated endpoint is that
`crates/stella-cli/Cargo.toml` stops declaring `stella-pipeline` — and a plugin
that is a workspace member is built, versioned and linked with the binary it
was supposed to leave. It would prove the opposite of what it is here to prove.

A plugin is a **separate program the host spawns**: argv from `[runtime]`, a
JSON request on stdin, a JSON response on stdout, an environment allowlist the
manifest names and a human consented to. Between those two it may *ask* the
host for a capability it cannot hold — the host-call channel, and only for the
capabilities `[loop] calls` declared (`doc:wrapper-socket` §6b). That is the
same contract a third-party plugin gets, and it is the contract these are
written against.

## Why they live here rather than in `stella-examples`

`macanderson/stella-examples` carries the *proof of the surface* — the
`verify-rs` / `verify-py` / `verify-ts` trio, one plugin written three times,
which is how "the plugin surface is a platform" stops being a claim
(`doc:pipeline-as-plugins` §9). Those are third-party-shaped by design.

These are different: a first-party stage extracted from the shipping pipeline,
which has to move in lockstep with this repository's wire contract. §9 rule 4
asks for exactly that — the reference plugins run "as a smoke check in `stella`
itself, so a protocol change that breaks a non-Rust plugin fails the PR that
made it rather than being discovered by a user". A vector living in another
repository cannot fail the PR that broke it.

## How they are graded, without being members

`cargo test --workspace` — the gate's `test` step and `ci.yml`'s required job —
runs the harnesses in `crates/stella-runtime/tests/`, which spawn these plugins
through the host's own `SubprocessWrapper` and grade their answers against
committed vectors. Nothing here is compiled by cargo; the plugins are data and
a program, and the test is the consumer. A diff that touches only `plugins/**`
is not a prose path, so it starts the Rust gate like any other change.
