# stella-selfdriving — the consent document, not yet the extraction

This directory holds one file that matters: `plugin.toml`, the declaration of
everything the perpetual delivery loop does to your machine.

```bash
stella plugin install plugins/stella-selfdriving
```

That prints the whole grant and asks. With no terminal attached it prints the
same text and refuses instead of assuming an answer.

## What this is

`doc:pipeline-as-plugins` §10 settles two things about self-driving, and this
package is built to both.

**It is a host, not a wrapper (option 2).** The loop does not participate in a
turn — it *drives* Stella from outside, calling the verbs
`stella self-driving surface` declares, exactly as `scripts/self-driving.sh`
already does (#1548). Forcing it through the turn-granularity wrapper socket
would widen that socket for a single caller. So the manifest declares
`participation = "none"` and no `[runtime]`, no `[oracle]`, no `[wrapper]`:
Stella never starts this program. A person does, and then it starts Stella.

**The authority question is settled** (run playbook §3, D-3). The loop already
holds `gh`, the AWS CLI, `brew`, a line in `~/.zshrc` and a daemon, today, as
a shell script running with your full authority. Packaging it **relocates**
that authority; it grants nothing new. What D-3 requires instead is that the
grant be *expressible* and *showable before install* — which is what this
directory is, and all it is.

## What this is not

**It is not the extraction.** `scripts/self-driving.sh` is still the working
driver and is deliberately untouched: §10's rule is that the shell driver is
not deleted until its replacement is proven, and no replacement has been
written. Installing this copies a declaration and this README. It starts
nothing.

**Nothing here is enforced.** Binding a declared capability to an `AuthzGate`
rule under `Principal::Plugin` is the loader's job
(`doc:pipeline-as-plugins` §A4) and has not landed for a host plugin. Read the
capability list as "what this loop does to your machine today, written down",
because that is exactly what it is — and read the install prompt's own
sentence about claimed limits, which says the same thing in Stella's words.

## What keeps it honest

`crates/stella-cli/tests/self_driving_consent.rs` renders this manifest
through the real `stella plugin install` and requires every power §10 names to
appear on **both** sides: in the text a user reads, and in
`scripts/self-driving.sh`, which is the program the text is a claim about. A
power the driver drops must leave the grant; a power the grant drops must
leave the driver.

That check is an enumeration, so it catches drift in a power somebody already
thought of. A driver that grew a capability nobody listed would pass it — the
fix for that is the loader gate above, not a longer list.
