---
id: adr/0034-silence-is-not-a-grant
title: "ADR 0034: Silence is not a grant"
status: implemented
---

# ADR 0034: Silence is not a grant

- Status: accepted
- Date: 2026-09-07
- Decides: `#6060`
- Not part of the Phase 0 series.

## Context

A plugin lists the host tools it wants in `[[capabilities]]`. A person reads
that list before the install. `PluginGates::from_roster` turns the list into a
rule. `PluginCapabilityGate::verdict` then refuses any tool the list left out,
and any tool this host grades above what the list took.

`from_roster` skipped a manifest whose list was empty. A plugin that asked for
nothing got no rule. Every tool call it made was allowed. Asking for less
bought more. That makes the list a thing an author is punished for writing.

The hole was live. `crates/stella-cli/src/candidate_workspaces.rs` runs a whole
writing worker turn as `Principal::Plugin`. The plugin that buys those turns,
`plugins/stella-candidates`, lists no capabilities at all.

Two real things blocked the fix.

**The prompt said the wrong thing.** `stella_plugin::consent_text` printed "It
asks for no tool capabilities." That is a line about the ask. A reader learns
what the plugin wanted and nothing about what it got. Refusing every call would
have enforced a rule no one saw.

**One name covered two callers.** `Principal::Plugin` was worn by a plugin's
own script tool and by a candidate worker turn. Those differ. A script tool is
the package's code reaching for the host's power. A candidate turn is the
user's own work. The session's model picks its tools. The session's tool policy
bounds them. It runs in a worktree of its own. To judge that turn by the
plugin's list, the list would have to name every tool a turn might reach, which
is every tool there is.

## Decision

**A plugin's grant is what its manifest declared and a person took.** That is
the `[[capabilities]]` list. It is also the script tools the package ships in
`[[tools]]`, and the namespace of each MCP server it ships in `[[mcp]]`. The
first is what the plugin asks of the host. The other two are the package's own
code. Each is named in its own table and shown at install. Refusing them would
break the tool a user just agreed to add. A shipped name joins the grant only
where the list is silent about it. So an author who grades their own tool is
still held to the grade the user read.

**An empty `[[capabilities]]` list grants nothing of the host's.**
`from_roster` builds a rule for every plugin on disk. Such a plugin gets a rule
that refuses every host tool. The refusal names the table an author would write
to widen it. `consent_text` prints the line this gate enforces.

**A worker turn the host runs for a plugin is a caller of its own.**
`Principal::PluginWorker(name)` is that caller. No capability rule matches it.
An operator gate may still write a rule about it. It can now tell that turn
from the plugin's own call. Nothing could do that before.

### What was given up

A **documented allow-all** was the cheap answer. Write down that silence grants
everything and stop calling it a hole. It keeps the reward backwards. An author
who lists three tools is bounded by three. An author who lists none is bounded
by nothing. No amount of writing makes that a fair rule for a market.

A **named default set** — a few low-risk tools every silent plugin gets, shown
at install — was turned down for two reasons. It is a list somebody must keep
up, and it will fall behind the tool surface it names. And each tool on it is
power handed to a plugin that never asked. A plugin that wants `read_file` can
write one line, and that line is a sentence a person reads.

The cost of the choice: a package that ships a tool or a server now carries its
own contribution in the grant. That is a second rule to know, past "the list is
the grant". The other option was to make each author name their own tool twice,
in `[[tools]]` and again in `[[capabilities]]`, and to break any package that
forgot.

## Consequences

- A plugin that listed no capabilities is refused a host tool. The witness is
  `plugin_authz::tests::a_plugin_that_declared_no_capabilities_is_granted_nothing`.
  The same call by the operator still runs, so the test cannot pass by refusing
  everything.
- A candidate worker turn still calls `bash`. The witness is
  `plugin_authz::tests::a_worker_turn_run_for_a_plugin_is_not_bound_by_the_plugins_tool_grant`.
  It asks one gate the same question twice and gets the two answers.
- A package's own tool still runs after install. That path was already covered
  end to end by
  `plugin_cmd::tests::contributions::a_plugins_tool_installs_runs_as_the_plugin_and_retracts_with_it`,
  which fails if the grant drops the shipped names.
- `HostDriverCapabilities::may_shell` asks this gate before the host runs a
  shell for a driver plugin. A driver plugin with no `bash` line is refused
  there too, and the reason says why.
- A session in a workspace with any plugin installed binds `PluginGates` rather
  than `NoAuthz`. The rule answers `Allow` for every caller it holds no rule
  about, so nothing else in the session moves.
- An operator gate sees two callers where it saw one. A rule about
  `Principal::Plugin` stops catching a candidate turn. That is the point, and
  it is a change for anyone who wrote such a rule against the old spelling.
