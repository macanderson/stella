# stella-selfdriving — the perpetual delivery loop, as a driver Stella starts

Two files matter here: `plugin.toml`, the declaration of everything the loop
does to your machine, and `main.py`, the program Stella runs against it.

```bash
stella plugin install plugins/stella-selfdriving
stella plugin drive stella-selfdriving
```

The install prints the whole grant and asks. With no terminal attached it
prints the same text and refuses instead of assuming an answer. The drive
opens one session against the program.

## What this is

`doc:pipeline-as-plugins` §10 settles two things about self-driving, and this
package is built to both.

**It is a host, not a wrapper (option 2).** The loop does not participate in a
turn — it *drives* Stella from outside. Forcing it through the
turn-granularity wrapper socket would widen that socket for a single caller.
So the manifest declares `participation = "none"` and no `[runtime]`, no
`[oracle]`, no `[wrapper]`. Its process is named in `[driver.process]`
instead, which is the block for a program that is never invoked inside a turn.

**The authority question is settled** (run playbook §3, D-3). The loop already
holds `gh`, the AWS CLI, `brew`, a line in `~/.zshrc` and a daemon, today, as
a shell script running with your full authority. Packaging it **relocates**
that authority; it grants nothing new. What D-3 requires instead is that the
grant be *expressible* and *showable before install* — which the manifest is.

## What the program does

`main.py` speaks the driver channel: one JSON message per line, an ask at a
time, and a `next` that ends the session. It holds no forge token, no provider
key and no worktree. Every capability it needs, it asks Stella for.

One cycle today: read the ranked queue (`backlog_next`), and if there is work
at the top of it, ask to claim it (`backlog_claim`). The claim is refused,
because no host serves it yet, and the program stops rather than working an
issue it cannot know is unclaimed. Two loops taking one issue is what a claim
prevents, and proceeding without one would trade a correct refusal for a
silent race.

## The grant binds

An ask outside `[driver] calls` comes back `err` with `refusal: "undeclared"`
and the session keeps going. That is the channel's own gate.

The tracker read goes through a second one. Stella performs it as
`Principal::Plugin("stella-selfdriving")`, and asks the rule
`crates/stella-cli/src/plugin_authz.rs` built out of the `[[capabilities]]`
list you accepted at install whether that principal was granted `bash` — the
capability that shells out to `gh`. A manifest without it is refused the read,
and the refusal names the plugin.

## What this is not

**It is not the whole extraction.** `scripts/self-driving.sh` is still the
working driver and is deliberately untouched: §10's rule is that the shell
driver is not deleted until its replacement is proven.

What has moved onto the channel: reading the ranked defect queue. What has
not: the claim, the worktree, the turn, the pull request, the merge, the
benchmark, the `brew` upgrade, the `~/.zshrc` line and the daemon. All of
those are still the shell script's, running as you, which is why the
`[[capabilities]]` list still declares them.

## What keeps it honest

`crates/stella-cli/tests/self_driving_consent.rs` renders this manifest
through the real `stella plugin install` and requires every power §10 names to
appear on **both** sides: in the text a user reads, and in the file that does
it. A power the loop drops must leave the grant; a power the grant drops must
leave the loop.

`crates/stella-cli/src/driver_plugin/tests.rs` drives `main.py` through the
real transport twice: once with the shipped grant, where the queue read is
served, and once with a grant that omits `backlog_next`, where the host refuses
it and the session still ends with a `next` instead of a crash.

The consent check is an enumeration, so it catches drift in a power somebody
already thought of. A driver that grew a capability nobody listed would pass
it — which is what the capability rule above exists to refuse.
