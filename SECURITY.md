# Security Policy

Stella runs shell commands, edits files, and talks to model providers on your
behalf — we take the security of that surface seriously, and we appreciate the
researchers who help keep it tight.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately via
[GitHub's private vulnerability reporting](https://github.com/macanderson/stella/security/advisories/new)
— it goes straight to the maintainers, and you get credit in the advisory when
it's published.

Include what you'd want in any good bug report: affected version/commit, a
reproduction, and your assessment of impact. We aim to acknowledge reports
within **72 hours** and to ship a fix or a mitigation plan within **30 days**
for confirmed issues, keeping you informed along the way.

## Scope — what counts

Especially interesting, given what Stella promises:

- **Workspace-root escape** — any way a tool call (file CRUD, `bash`, `grep`/`glob`)
  reaches outside the pinned workspace root: traversal, symlinks, race conditions.
- **Phone-home violations** — telemetry, update checks, or analytics leaving the
  machine in Community/default mode. Zero is the contract there, and the only
  governed exception is an explicitly enrolled Oxagen Enterprise seat. Network
  traffic the user asked for is *not* a violation: the chosen model provider,
  configured MCP servers, the opt-in `web` tools (`web_fetch` /
  `web_extract_assets` / `web_download`, and `web_search` against your own
  Brave/Tavily key), and `gh`/Linear when an issue backend is connected. Traffic
  from any of those to a host the user did not configure *is* in scope.
- **Credential exposure** — API keys leaking into logs, telemetry, error
  messages, or files with permissive modes.
- **Prompt/tool injection with impact** — untrusted content (repo files, MCP
  frames, provider responses) escalating into actions the user didn't sanction,
  beyond what the model is already trusted to do.
- **Context Graph Protocol (CGP) host boundary breaks** — providers escaping quarantine: inheriting
  credentials, ambient filesystem access, or ungated egress.
- **install.sh / release integrity** — checksum bypasses, tag/asset confusion.

Out of scope: vulnerabilities in the model providers themselves, and the
inherent risk of running an agent with `bash` access on code you don't trust —
that's the user's judgment call, not a boundary Stella claims to enforce.

## Threat model

`docs/spec/threat-model.md` enumerates the assets, the adversaries, the
trust boundaries, and the attack paths that cross them — including the risks
Stella knowingly does not defend against, and why. Read it before deciding
whether a behavior you found is a vulnerability or a documented choice: several
of the sharper edges (no in-process confinement on any spawned command since
the per-command sandbox was removed, the `web` egress guard
being bypassed by an HTTP proxy, materially weaker guarantees off Unix) are
deliberate and recorded there.

## Verifying a release

Every artifact published by `.github/workflows/release.yml` carries three
independent guarantees, and they answer different questions:

- **`SHA256SUMS`** proves the tarball was not corrupted or truncated. It is
  fetched from the same release over the same channel as the artifact it
  vouches for, so it does *not* establish origin — anything able to replace one
  could replace both.
- **A Sigstore build-provenance attestation** (`actions/attest-build-provenance`)
  proves the artifact was built by this repository's release workflow at a
  specific commit. It covers the tarballs *and* `SHA256SUMS`, and it cannot be
  reissued by whoever holds the release. No signing key is involved, so there
  is none to leak or rotate.
- **A reproducible build** proves the artifact corresponds to the published
  *source*. `scripts/repro-build.sh` remaps the builder's `$CARGO_HOME` and
  rustup sysroot out of the binary and refuses to run on anything but the
  `rust-toolchain.toml` pin, so anyone can rebuild a tag and get our bytes;
  release.yml rebuilds one target on a second runner with a deliberately
  different environment and blocks publication if the two disagree. The
  per-target binary hashes are published as `SHA256SUMS.bin` — see
  [RELEASING.md](RELEASING.md) for the recipe. This is the guarantee the other
  two cannot give: an attestation proves *we* built it, not that what we built
  is what we published the source for.

`install.sh` checks the tarball checksum always, checks the binary against
`SHA256SUMS.bin` when the release publishes one, and checks provenance whenever
`gh` is on `PATH`. A verifier that runs and **rejects** an artifact is always fatal. A
verifier that cannot run (no `gh`, a `gh` too old, or a release predating
provenance) is a warning — so to demand the strong guarantee, ask for it:

```bash
STELLA_REQUIRE_PROVENANCE=1 curl -fsSL https://raw.githubusercontent.com/macanderson/stella/main/install.sh | sh
```

To verify an already-downloaded artifact by hand:

```bash
gh attestation verify stella-<version>-<target>.tar.gz --repo macanderson/stella
```

Releases cut through the degraded local path (`scripts/release.sh`, used only
when GitHub Actions cannot run — see RELEASING.md) carry **no attestation** and
will be refused by `STELLA_REQUIRE_PROVENANCE=1`. Those releases say so in their
notes.

A binary that fails provenance verification is a report-worthy event under
"install.sh / release integrity" above — please do not install it.

## Supported versions

Pre-1.0, only the latest release (and `main`) receive security fixes.
