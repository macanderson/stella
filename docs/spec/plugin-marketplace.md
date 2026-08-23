---
id: plugin-marketplace
title: "The plugin marketplace: an index is a repository, not a service"
status: proposed
---

# The plugin marketplace

**Status:** proposed, written 2026-08-19, against `main` at `f9789df17`.
Companion to `doc:plugin-completion-plan`, which specifies the plugins
themselves; this document specifies how anybody gets one.

**The recommendation in one paragraph.** Do not build a registry service.
Make the marketplace a **git repository containing an index of plugin
listings**, resolve installs to **pinned commit SHAs with recomputable tree
digests**, let anyone add their own index ("tap") so distribution needs
nobody's permission, generate a **static discovery site** from the index at
merge, and make the one genuinely novel thing a **grant diff on upgrade** — a
plugin may never quietly widen what it asked for. This costs one repository and
a static site build, breaks none of Stella's invariants, requires no accounts,
works offline after the first fetch, and can grow a hosted API later without
changing how anything resolves.

---

## 1. What exists today

Read out of the tree, because the gap is the design input:

- **`stella plugin` has exactly three subcommands** — `Install { dir, scope,
  yes }`, `List`, `Remove { name }`
  (`crates/stella-cli/src/plugin_cmd.rs:48-68`). `install` takes a **local
  directory** and copies the tree (`stage_and_commit`, `copy_tree`, `:520`,
  `:577`). There is no network path of any kind.
- **The manifest carries no distribution identity.** `PluginManifest`
  (`crates/stella-plugin/src/manifest.rs:449`) has fourteen fields: `name`,
  `description`, `loop`, `driver`, `requirements`, `oracle`, `subloop`,
  `roles`, `wrapper`, `runtime`, `capabilities`, `tools`, `skills`, `records`.
  **No `version`. No `license`. No `publisher`. No `homepage`. No declaration of
  which wire protocol it speaks.**
- **The wire is versioned; the manifest is not.** `PROTOCOL_VERSION: u32 = 1`
  rides on every message (`crates/stella-plugin/src/wire.rs:79`), so an
  incompatible plugin is discovered at first dispatch rather than refused at
  install.
- **Install is already a consent transaction, and that is the asset.**
  `plugin_cmd.rs`'s own header says so, and `stella_plugin::consent_text` is a
  pure function over the manifest's bytes — one document, rendered by the crate
  rather than by the CLI, precisely so *"`stella plugin install` was complete
  and every embedding host was not"* stopped being true (#3565,
  `doc:pipeline-as-plugins` §13.2).
- **Retraction is already structural.** Nothing is copied into `.stella/tools`,
  `.stella/skills` or `.stella/rules`; contributions are recomputed from
  installed packages on every load, so `remove` deletes a directory and there is
  nothing left to forget (§13.2). **Uninstall therefore survives any transport
  we choose**, which is a large piece of the problem already solved.

So: consent and retraction are done and done well. **Identity, integrity,
versioning, discovery and distribution do not exist at all.**

---

## 2. The five constraints that decide the design

These are not preferences. Each one eliminates at least one otherwise-reasonable
option.

**C1 — Zero telemetry egress by default (invariant 3).** *"Update checks and
anonymous analytics remain prohibited."* A registry that is consulted in the
background, that counts installs, or that must be reachable for Stella to
function, breaks the invariant the project enforces with a reviewed allowlist
and a gate. Any network call must be user-initiated, to a host the user can
name.

**C2 — The manifest is the product, and a human reads it before code runs.**
Whatever the transport, the artefact a user consents to is the grant. A
marketplace that surfaces stars and download counts more prominently than the
grant is optimising the wrong thing for this product.

**C3 — A plugin is code that runs with the operator's authority, today.**
Nothing constructs `Principal::Plugin` for a capability binding (#3482), so a
declared `[[capabilities]]` entry is a claim, not a fence.
`doc:pipeline-as-plugins` §A1: *"a marketplace shipped on top of a system that
cannot distinguish an installed plugin from its operator grants every plugin the
operator's authority."* **This bounds how open distribution may be until #3482
lands** (§7).

**C4 — Stella has no accounts and should not grow them for this.**
`~/.stella/cloud.json` reserves an `oauth_token` slot that nothing fills. A
design requiring publisher accounts, sessions and password resets is a
different product.

**C5 — Offline and air-gapped use must keep working.** Stella is a local binary
run by people on planes and inside networks that do not reach the public
internet. Resolution that requires a live service is a regression for those
users.

---

## 3. Four options, honestly compared

| | **A. Registry service** (crates.io / npm) | **B. Git-native + index repo** (Homebrew tap / Go) | **C. OCI artifacts** (ghcr + cosign) | **D. B, plus a static discovery site** |
|---|---|---|---|---|
| Infrastructure to run | a service, a database, object storage, on-call | **none** | none (the OCI registry is somebody else's) | a static build, on the docs deploy that already exists |
| Accounts needed | yes (C4 ✗) | no | no (uses registry auth) | no |
| Works offline after first fetch | no (C5 ✗) | **yes** | partly | **yes** |
| Background egress | typical (C1 ✗) | none | none | none |
| Publish flow for an author writing `main.py` | `stella plugin publish` | `git tag` + one PR | `oras push` + cosign (a wall) | `git tag` + one PR |
| Who reviews a new listing | whoever staffs the registry | **a PR, with a diff and a history** | nobody | a PR |
| Discovery | strong | weak | none | **good** (static site + offline `search`) |
| Content integrity | server-attested | pinned SHA + tree digest | content-addressed (best) | pinned SHA + tree digest |
| Takedown / revocation | delete the crate | yank flag in the index | delete the tag | yank flag in the index |
| Cost to Stella of getting it wrong | a 24/7 security duty | a bad PR merge | a Rust OCI client dependency | a bad PR merge |

**A is rejected on C1, C4 and C5, and on a cost nobody mentions until they own
one: a package registry is a permanent security obligation** — typosquatting,
malware review, key rotation, abuse reports, legal takedowns — staffed
continuously. Stella should acquire that obligation when the ecosystem's size
forces it, not before.

**C is technically the best of the four and rejected on adoption.** Content
addressing and cosign signing are exactly right. But the plugin surface's whole
argument (`doc:pipeline-as-plugins` §9 rule 3) is *"no SDK in the first cut… if
a plugin cannot be written without an SDK, the protocol is too complicated."*
Requiring `oras` and `cosign` to ship a Python file contradicts that argument at
the distribution layer. Keep C in reserve: the index format in §4 can add an
`oci` reference beside `commit` later without changing anything else.

**B is the design; D is B finished.** Homebrew ran at enormous scale for years
with no registry service, and Go removed the central index from module
resolution entirely. Both worked for the same reason: **git already is a
content-addressed distribution network with authentication, mirroring and
history, and every plugin author already has one.**

---

## 4. The design

### 4.1 An index is a repository

The default index is `oxageninc/stella-registry` — public, one file per plugin
under `plugins/<name>.toml`:

```toml
name = "stella-lint"
description = "Hold the turn open until the linter is clean."
publisher = "acme"
repository = "https://github.com/acme/stella-lint"
homepage = "https://acme.dev/stella-lint"
license = "Apache-2.0"
categories = ["verification", "code-quality"]

[[releases]]
version = "1.2.0"
git_ref = "v1.2.0"
commit = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
tree_digest = "sha256:9f86d0818…"
protocol = 1
yanked = false

[[releases]]
version = "1.1.0"
git_ref = "v1.1.0"
commit = "0f1e2d3c4b5a69788796a5b4c3d2e1f001234567"
tree_digest = "sha256:2c26b46b6…"
protocol = 1
yanked = true
yank_reason = "leaked an API key into the consent text"
```

Five properties, each required:

1. **The index pins a commit SHA, never only a tag.** Tags move. This is Go's
   lesson and it costs nothing to adopt.
2. **`tree_digest` is recomputed by the host after copy and compared.** That is
   integrity without any signing infrastructure at all: the trust event is the
   index PR, and after it the bytes are pinned forever. A repository owner who
   force-pushes over a tag cannot change what an installed plugin is.
3. **`protocol` is the wire version the plugin speaks**, so an incompatible
   plugin is refused **at install** rather than at first dispatch. This needs
   prerequisite M-3 (§5).
4. **`yanked` is the revocation channel** — the index cannot delete anything
   from anyone's disk, and should not pretend to; what it can do is make
   `stella plugin outdated` say so loudly and make new installs of that version
   refuse.
5. **Adding a plugin is a pull request.** Author, diff, review, history, revert.
   Most registries reconstruct a worse version of this after the fact.

### 4.2 Anyone may run an index — "taps"

```bash
stella plugin tap add acme https://github.com/acme/stella-taps
stella plugin tap list
stella plugin tap update            # explicit refresh; nothing polls
stella plugin tap remove acme
```

A tap is a git repository with an `index/` directory in the §4.1 format,
cloned to `~/.stella/taps/<name>/`. The default `oxagen` tap is **seeded and
removable**. An enterprise adds a private tap and its people install internal
plugins with no Oxagen involvement, no allowlist request and no egress outside
their own network — which is not a concession to enterprises, it is the same
property that makes the design honest for everyone: **nobody needs anybody's
permission to distribute a Stella plugin.**

Names resolve tap-first when ambiguous (`acme/stella-lint`), and a bare name
that matches in two taps is an error naming both, never a silent pick.

### 4.3 Resolution and the lockfile

`stella plugin install <source>` takes one of three sources, which **scope** the
verb rather than select a different one (invariant 9):

| Source | Example | Meaning |
|---|---|---|
| a local path | `stella plugin install ./plugins/stella-research` | today's behaviour, unchanged |
| a git reference | `stella plugin install git+https://github.com/acme/stella-lint@v1.2.0` | no index involved |
| an index name | `stella plugin install stella-lint@1.2.0` | resolved through the taps |

Every install writes a **lockfile** — `.stella/plugins.lock` for the project
tier, `~/.stella/plugins.lock` for the user tier:

```toml
[[plugin]]
name = "stella-lint"
source = "tap:oxagen"
version = "1.2.0"
commit = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
tree_digest = "sha256:9f86d0818…"
consented_grant = "sha256:60303ae22…"   # digest of the rendered consent_text
installed_at = "2026-08-19T18:04:11Z"
```

One file, four jobs, and this is where most of the design's value is:

- **Reproducible installs.** `stella plugin sync` installs exactly what the
  lockfile says — how a teammate or a CI job gets the same plugins, and the
  reason the lockfile belongs in git for a project-tier install (the same
  reasoning that makes `.stella/rules/` tracked: *"a record only steers a
  teammate's session if it travels with the repository."*)
- **Integrity re-checking.** `stella plugin verify` recomputes every installed
  tree's digest against the lockfile. A plugin edited in place after consent is
  detectable.
- **The upgrade base.** `consented_grant` is what §4.4 diffs against.
- **Provenance in `stella plugin list`**, which today cannot say where a plugin
  came from or what it configured (#4018).

### 4.4 The grant diff on upgrade — the part that matters most

**#3514 is open and says a plugin can widen its own grant by rewriting its
manifest.** In a world of local directory installs that is a footgun. In a world
of distributed, upgradable plugins it is the primary attack: version 1.2 asks
for `Stop` and `bash`; version 1.3 quietly adds `deliver_merge` and a wider
environment allowlist; the user typed `upgrade` and read nothing.

So `stella plugin upgrade` is a **separate verb from `install`** — not a flag —
because it has a gate `install` does not:

1. Render `consent_text` for the new manifest.
2. Diff it against the `consented_grant` recorded at install.
3. **Unchanged or narrower → proceed, printing the one-line summary.**
4. **Wider → print only what was added, and require an explicit yes.** With no
   terminal attached, **refuse** — the same posture
   `plugins/stella-selfdriving`'s README already describes for install: *"With
   no terminal attached it prints the same text and refuses instead of assuming
   an answer."*

This converts consent from a one-time event into a maintained invariant, and it
is nearly free: `consent_text` is already a pure function over manifest bytes,
so the diff is a diff of two strings the crate already knows how to produce.
`crates/stella-diff` is a leaf with no dependencies and renders exactly this
shape.

**Witness:** a plugin whose 1.3.0 manifest adds one `[[capabilities]]` entry
upgrades only after an explicit yes, and refuses under `--yes` unless the flag
is the narrower `--accept-widened-grant`; a 1.3.0 that *removes* a capability
upgrades silently. Both directions, because either alone is half a gate.

### 4.5 Discovery is a static site, not an API

The index repository builds a static site at merge — the same Vercel deploy
path the documentation site under `website/` already uses. One page per plugin,
and the page's centrepiece is **the
rendered consent text for the latest release**, so a person can read exactly
what a plugin will ask for *before downloading anything at all*. That is the
inversion this product should be known for: every other marketplace shows you
the pitch and hides the permissions.

In the CLI, discovery is **offline over the cached taps**:

```bash
stella plugin search lint          # substring/category match over cached indexes
stella plugin info stella-lint     # the listing, the versions, and the grant
```

No search API, no server, no egress beyond the explicit `tap update`. This is
the same posture `stella models` already takes toward the model catalog — ship
the data, never phone home.

### 4.6 Publisher identity, phased

**v1 — the namespace is claimed in the index, and the PR is the identity.** A
`publisher` value is owned by a GitHub org or user recorded in the index repo's
`publishers/<name>.toml`, and `CODEOWNERS` rejects a PR touching another
publisher's listings. This is cheap, real, reviewable, and requires no keys.

**v2 — signed tags, once v1 exists.** The listing records an expected signing
identity (a sigstore identity or an OpenPGP fingerprint) and the host verifies
the tag signature at install. Deliberately **not** first: *a signature over an
unclaimed name proves that somebody signed something.* The namespace claim is
what makes the signature mean anything, and it is the cheaper half.

### 4.7 Paid plugins with no payments infrastructure

**The git remote is the paywall.** A listing may declare:

```toml
distribution = "licensed"
license_url = "https://oxagen.com/vera"
```

The repository is private. The buyer receives read access — an org invitation,
a deploy key, a token — and `stella plugin install vera` resolves the listing,
sees `licensed`, and clones using the **user's existing git credentials**. If
they have no access, the refusal names `license_url` instead of a 404.

Oxagen builds zero payments infrastructure: Stripe plus a GitHub org invite is
the entire fulfilment path, and Vera ships the day it is written. It generalises
to third-party paid plugins later without Stella becoming a payment processor —
a role that brings tax, chargeback, refund and compliance obligations that would
dominate the roadmap of a team this size.

---

## 5. Prerequisites — none of the above is buildable without these

| | Prerequisite | Why it blocks | Related |
|---|---|---|---|
| **M-1** | `version` (semver, required for a published plugin), `license` (SPDX), `publisher`, `homepage`, `repository` on `PluginManifest` | there is nothing to index, compare, upgrade or attribute without them | new |
| **M-2** | A tree-digest function over an installed package, and `.stella/plugins.lock` | integrity, `sync`, `verify`, and the upgrade base (§4.3) | new |
| **M-3** | A manifest declaration of the wire `PROTOCOL_VERSION` it speaks, and an install-time refusal on mismatch | today an incompatible plugin fails at first dispatch, which for a *distributed* plugin means after consent and after a paid model call | new |
| **M-4** | `stella plugin list` reports source, version and what the package configured | provenance is unreadable today | #4018 |
| **M-5** | `Principal::Plugin` constructed, `[[capabilities]]` bound to an `AuthzGate` rule | **gates how open distribution may be** (§7) | #3482 |
| **M-6** | Uninstall removes a symlinked install | a distribution channel that can install what it cannot remove is not one | #3530 |

M-1 through M-3 are additive manifest and CLI work with no architectural
tension; they are the first thing to build and they are useful on their own,
before any index exists. M-5 is the one that decides §7.

---

## 6. Phases

Each phase is independently shippable and independently useful. Nothing here
requires the index to exist until M-5's phase.

- **P1 — identity.** M-1 + M-3. *Witness: a manifest missing `version` installs
  from a local path (unchanged) and is refused a published listing; a manifest
  declaring `protocol = 2` against a host at `PROTOCOL_VERSION = 1` is refused
  at install, naming both numbers.*
- **P2 — integrity and reproducibility.** M-2 + `stella plugin verify` +
  `stella plugin sync`. *Witness: a byte edited inside an installed package
  makes `verify` fail and name the file; `sync` on a clean checkout reproduces
  the locked set exactly.*
- **P3 — remote install, no index.** `git+<url>@<ref>`. Resolution pins the
  SHA into the lockfile. *Witness: two installs of the same moving tag record
  the same commit and refuse the second if the tree digest changed.*
- **P4 — upgrade with the grant diff.** §4.4. Closes the distribution half of
  #3514. *Witness: §4.4's pair.*
- **P5 — taps and the index.** `tap add|list|remove|update`, `search`, `info`,
  `outdated`, and the `oxageninc/stella-registry` repository with its schema
  check in CI. *Witness: a listing whose `tree_digest` does not match its
  `commit` fails the index repo's own gate, on the PR that added it.*
- **P6 — the static discovery site**, built from the index at merge, rendering
  the consent text per listing.
- **P7 — licensed distribution.** §4.7, with Vera as its first and proving
  listing.
- **P8 — open the default tap to third-party submissions.** **Gated on M-5
  (#3482)** — see §7.

---

## 7. The security position, stated in the terms it is actually true in

A plugin is executable code that today runs with the operator's authority,
because nothing binds a declared capability to a gate (#3482). Two consequences
this document will not soften:

**The default tap stays curated until #3482 lands.** First-party plugins and
reviewed partners only. Third parties distribute through their own taps in the
meantime, which is not a second-class path — it is the *honest* one, because
adding a tap is a user explicitly choosing a source, whereas a public default
index implies a safety property Stella cannot yet provide.

**The install prompt says the true thing about review.** When a marketplace
listing is involved, the consent text must not imply that listing means vetting.
The index reviewed the **listing** — the name, the namespace claim, the pinned
commit — and not the **behaviour**. `doc:pipeline-as-plugins` §13.3 already sets
this standard for the manifest's own claims: *"printing an unverified command
line is the 'claimed limit rendered as an enforced one' mistake in new
clothes."* The same rule governs the word "verified" on a marketplace page.

**After #3482, the position improves but does not become "safe."** A capability
gate makes a declared grant enforceable; it does not sandbox a process that was
granted `bash`. The honest ceiling for the foreseeable future is *"you can read
exactly what it asked for, the host enforces exactly that, and the bytes are
pinned."* That is a considerably better story than every incumbent tells, and
overstating it by one word would forfeit the advantage.

---

## 8. What this design deliberately does not do

- **No download counts, no popularity ranking.** Counting installs is
  telemetry, and invariant 3 does not have an exception for flattering numbers.
- **No ratings or reviews.** They need accounts (C4) and they are the part of
  every marketplace that gets gamed first.
- **No `stella plugin publish`.** Publishing is `git tag` plus a pull request.
  A CLI verb would add a credential path and an upload surface to replace two
  commands an author already knows.
- **No inter-plugin dependency resolution.** Plugins are leaves. Two plugins
  that need to compose is #3801's problem — one `--pipeline` selection binding
  more than one manifest — and a version solver would be the wrong tool
  answering the wrong question.
- **No auto-update.** `outdated` reports; a human upgrades. An agent that
  silently changed what it is allowed to do between two runs is the exact thing
  the grant diff exists to prevent.

---

## 9. Risks

- **A deleted upstream repository breaks reinstall.** Real, and mitigated only
  partly: the lockfile pins digests so *existing* installs keep working and stay
  verifiable, and a tap may optionally vendor a mirror. Not solved, and named
  rather than hidden — it is the standing cost of choosing git over an
  artifact store, and the point at which option C (§3) earns a second look.
- **A tap is a trust root, and adding one is a security decision.**
  `tap add` must print what it means in a sentence, the way install already
  does.
- **Discovery is weaker than a hosted registry's.** Accepted at the scale of
  dozens to hundreds of plugins. The index stays canonical if a search API is
  ever added, so this is a deferrable decision rather than a one-way door — the
  property §3 selected B for in the first place.
- **The grant diff is only as good as `consent_text`'s completeness.** It is
  complete today because `PluginManifest::reconcile` refuses both an undeclared
  directory and an undelivered declaration (§13.3). **A future manifest field
  that `consent_text` does not render silently becomes un-diffable**, so a
  test asserting that every manifest field reaches the consent document should
  land with M-1, not after it.
