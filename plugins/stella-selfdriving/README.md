# stella-selfdriving — the perpetual delivery loop, as a driver Stella starts

Two files matter here: `plugin.toml`, the declaration of everything the loop
does to your machine, and `main.py`, the program Stella runs against it.

```bash
stella plugin install plugins/stella-selfdriving
stella plugin drive stella-selfdriving
```

The install prints the whole grant and asks. With no terminal attached it
prints the same text and refuses instead of assuming an answer. The drive
opens a session against the program, re-opens it whenever the program asks to
sleep, and stops when it halts — or when `--spend-limit` or `--max-sessions`
is reached.

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
(`backlog_claim`), ask Stella to work that issue (`work_start`) in a checkout
of its own, open the pull request (`deliver_open`), read the forge
(`deliver_observe`), and ask what to do next (`deliver_next`). It will not work
an issue it cannot know is unclaimed — two loops taking one issue is what a
claim prevents, and proceeding without one would trade a correct refusal for a
silent race.

The cycle ends where the decision does. A pull request waiting on CI, needing a
fix, or needing a rebase is a later cycle's work, so this one sleeps and says
which state it stopped in.

Two decisions it acts on. `deliver_open` opens a draft, so the first answer for
a green pull request is `mark_ready` — the cycle takes it out of draft with
`deliver_ready` and reads once more. A decision of `merge` it acts on with
`deliver_merge`.

## The mark-ready and the merge are Stella's call

`deliver_next` decides over facts this program sends it, which is what makes it
cheap: a cycle that already read the forge does not pay to read it twice.

`deliver_ready` and `deliver_merge` do not work that way. Each ask names a pull
request and carries no facts, and Stella reads the forge itself and runs the
same machine over its own answer before acting. A cycle that reported a green
build nobody saw gets a refusal naming the state Stella found. That is what
makes putting a merge on a plugin channel safe — the branch protection your
repository declares is read by the host, and no message this program writes
reaches past it.

The draft is why `deliver_ready` is held to the same rule. A pull request opens
as a draft so one that never goes green never asks a human to look at it, and
taking it out of draft on this program's word alone would spend that.

A human still approves. The channel has no way to say otherwise. An operator
who wants their own loop merging unreviewed work says so to
`stella self-driving drive --no-review`, by hand.

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
cooperative claim, the worktree and the turn behind `work_start`, and the pull
request from `deliver_open` through the mark-ready to `deliver_merge`. What has
not: the
sweep, the benchmark, the `brew` upgrade, the `~/.zshrc` line and the daemon.
All of those are still the shell script's, running as you, which is why the
`[[capabilities]]` list still declares them.

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

`crates/stella-cli/src/driver_plugin/tests/deliver.rs` holds the merge to its
own rule. A forge answering red refuses the merge and nothing reaches the
forge; a forge answering green and approved merges the number the ask named;
and a green build nobody reviewed waits.

The consent check is an enumeration, so it catches drift in a power somebody
already thought of. A driver that grew a capability nobody listed would pass
it — which is what the capability rule above exists to refuse.
