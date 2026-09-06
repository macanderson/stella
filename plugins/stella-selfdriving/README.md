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

One cycle today: read the ranked queue (`backlog_next`), claim the top of it
(`backlog_claim`), and ask Stella to work that issue (`work_start`) in a
checkout of its own. It will not work an issue it cannot know is unclaimed —
two loops taking one issue is what a claim prevents, and proceeding without one
would trade a correct refusal for a silent race.

The cycle ends at the diff. No host serves `deliver_open` yet. So the `halt`
names the branch the work sits on. A person, or the shell script, opens the
pull request.

## The grant binds

An ask outside `[driver] calls` comes back `err` with `refusal: "undeclared"`
and the session keeps going. That is the channel's own gate.

The tracker read and the work go through a second one. Stella performs each as
`Principal::Plugin("stella-selfdriving")`, and asks the rule
`crates/stella-cli/src/plugin_authz.rs` built out of the `[[capabilities]]`
list you accepted at install whether that principal was granted `bash` — the
capability that shells out to `gh`. A manifest without it is refused both, and
the refusal names the plugin.

A `work_start` spends your provider budget. You set the ceiling:
`stella plugin drive stella-selfdriving --spend-limit 25`. With no ceiling,
spend is added up and reported, and nothing is refused. That is what
`stella self-driving` does with the same flag absent.

## What this is not

**It is not the whole extraction.** `scripts/self-driving.sh` is still the
working driver and is deliberately untouched: §10's rule is that the shell
driver is not deleted until its replacement is proven.

What has moved onto the channel: reading the ranked defect queue, the
cooperative claim, and the worktree and the turn behind `work_start`. What has
not: the pull request, the merge, the sweep, the benchmark, the `brew` upgrade,
the `~/.zshrc` line and the daemon. All of those are still the shell script's,
running as you, which is why the `[[capabilities]]` list still declares them.

## What keeps it honest

`crates/stella-cli/tests/self_driving_consent.rs` renders this manifest
through the real `stella plugin install` and requires every power §10 names to
appear on **both** sides: in the text a user reads, and in the file that does
it. A power the loop drops must leave the grant; a power the grant drops must
leave the loop.

`crates/stella-cli/src/driver_plugin/tests.rs` drives `main.py` through the
real transport twice. Once with a grant that carries the read and the claim:
both are served, and the program gets as far as asking for the work. Once with
a grant that omits `backlog_next`: the host refuses it, and the session still
ends with a `next` rather than a crash.

The consent check is an enumeration, so it catches drift in a power somebody
already thought of. A driver that grew a capability nobody listed would pass
it — which is what the capability rule above exists to refuse.
