# Examples

Finished prose per surface. Copy the structure, not the sentences, unless the sentence is an approved line. Approved lines live in the message registry at `messages/` in `oxagenai/oxagen-brand`, and `positioning.md` lists the current ones. Every control claim below carries its scope. Keep the scope when you shorten a line.

## Website hero

**Eyebrow.** The control plane for your agent workforce

**Headline.** Mission Control for your autonomous agents.

**Sub.** Give each agent an identity. Set its authority, budget, tools, and skills. Define completion when the work has an endpoint, and oversee ongoing work through its requests, activity, and spend. Oxagen keeps the record of the actions it governs.

**Action.** Explore Mission Control

**Under the action.** Follow an agent from its assigned authority to a recorded action.

The destination is a walkthrough with an agent's identity, its mandate, a tool and skill assignment, an allowed action, a request for approval, and the resulting record, plus an ongoing agent and a bounded task with completion criteria. An action leads to the experience it promises.

## Product page, the authority section

### Set the authority behind each action

Give each agent its own identity in your identity and access management system. Set which actions it may request and which resources it may reach. For actions handled through Oxagen, rules allow the request, deny it, or route it to a person. The record connects the decision to the agent and the person responsible for its work.

The detail page names the supported identity systems and explains provisioning, delegation, revocation, and where enforcement stops.

## Product page, the access section

### Don't hand your agents the keys

An agent under Oxagen has its own identity and a mandate. For the systems Oxagen mediates, the agent does not hold a standing token or password. When a task needs one of those systems, the agent asks. The request carries who started the task, which agent is asking, which tool it wants, and which data it would reach.

### A rule you wrote answers

The team that owns the system writes the rule. Reads of the public repo: allowed. A push to main: denied. A push to a release branch: routed to the release owner, who sees the request with the diff beside it and answers from the Access page. Allowed and denied requests settle inside the same call. Routed requests wait for the person, and the run waits with them. When a workspace has no rules, requests are answered by identity alone, and the Access page says so.

### For mediated connections, the credential stays in Oxagen

The connection is held by the workspace, encrypted under a key you own. Oxagen uses the connection credential on the agent's behalf, and the agent does not receive it. The agent's own identity and authentication are separate from that credential. The record shows every governed use: the request, the rule that answered it, and the person who signed it.

## Product page, human approval

### Put the decision in the right hands

Your rules decide when a person needs to look. Oxagen pauses the governed request and shows the reviewer which agent asked, what it wants to do, and the authority it would use. The run resumes with the answer recorded. Routine requests proceed under the rules you already set.

## Product page, the equipment section

### Give each agent the tools and skills its work requires

Manage the tools and skills available to your workforce, then assign them through each agent's mandate. Pair the equipment with the authority to use it and the business context the agent is permitted to read.

A tool in the catalog does not grant every agent permission to use it. Each request still meets your rules. The detail page shows how an operator changes an assignment, which version an agent receives, and when the change takes effect.

## Product page, the work section

### Define completion when the work has an endpoint

For a bounded task, set the checks and human decisions that determine completion before the agent starts. Oxagen locks those criteria into the run and records the results. For an ongoing responsibility, set the authority, budget, and review points that keep the work accountable over time.

A passing verdict means the specified checks held. Your team decides whether those checks are sufficient for the task. When checks fail, the record shows the reasons and whether the agent continued, waited for a person, or reached its retry limit.

## Product page, the operations section

### Manage the workforce from Mission Control

See the agents enrolled in Oxagen, the mandates they work under, their open requests, and their recorded activity and spend. Answer an approval request, change an assignment, or adjust a budget from the operator's view. Use the hold and stop controls supported by each integration.

Agents run in their supported environments. Oxagen is the control plane that governs their work. Each control says what it changes and when it takes effect.

### The Fleet page

An agent waiting for approval. Another nearing its budget. A third working from the wrong assumption. The Fleet page brings their runs, requests, mandates, and spend into one view, whoever built the agent, for every agent enrolled in Oxagen. Open the run, read what happened, and take the next action with the record beside you.

### Change course while the work is happening

Open a run and send the correction where it matters. Deliver steering after the current step, at the next turn, or by interrupting where the connection supports it. Oxagen records the instruction and its delivery status, so you can tell whether it is still waiting or has entered the agent's context.

### Stop the next action at the boundary

When a run needs attention, pause it at the next supported boundary. When authority needs to end, revoke it at the scope that fits: one agent, one connection, a class of tools, or the organization. Governed calls meet the new restriction before proceeding, and the affected runs show the reason.

## Product page, the audit section

### Follow the action back to its authority

Review which agent requested an action, on whose behalf, which rule answered, and who approved it when a person was required. Inspect the recorded result and cost alongside that decision.

For runs with completion checks, export the record so another person can recompute the recorded verdict without an Oxagen account or network connection. An exported verdict does not establish that every requirement was captured or that every action was correct.

### Hand over the evidence with the answer

Oxagen links recorded frames in a hash chain and signs the run's close. An exported record lets a reviewer check its integrity outside the product. It identifies what the gateway observed, what the agent's wrapper reported, and where evidence is missing, so the strength of the record stays visible.

### See what the agent saw

Scrub to the moment a run changed direction. Where the recording retains full content, read the context the agent received, what it asked, what came back, and what it had spent by then. Each run shows the playback its evidence supports.

## Security page

### Identity, authority, and a record you can inspect

Review how agent identities map to your access controls, how Oxagen handles connection credentials, and what each governed action records. Inspect the supported deployment and integration boundaries before granting authority.

For mediated connections, Oxagen uses the connection credential on the agent's behalf without giving that credential to the agent. The agent's own identity and authentication are separate from the downstream credential held for a connection.

The SOC 2 paragraph states the actual report status and scope, following the publication rule in `positioning.md`. Until a report supports it, this page makes concrete control claims and no attestation claim.

## Finance entry point

### See which agent spent what, and on whose behalf

Review recorded costs by person, agent, and run. Inspect the usage and rate behind a charge, then use that record to decide where to change a budget.

**Action.** Inspect workforce spend

### Follow spend down to the work

Open a spend total and keep going until you reach the work behind it. Oxagen records model usage by token class, the rate applied, and the agent and operator responsible. Finance and engineering read the same underlying records, with measured, reported, and estimated costs identified.

### Give every agent a spending limit

Set budgets by agent, operator, workspace, or organization. Oxagen checks hard budgets before model calls routed through its proxy and applies supported controls through the wrapper. A budget-triggered pause appears in the run with its reason, so the operator can decide whether to fund more work or stop there.

Savings language appears on this page only beside the measured workload and conditions.

## Knowledge entry point

### Give agents the business context their work requires

Put approved business context within the agent's permitted knowledge scope. Define which sources it can use and review those sources as the business changes. Oxagen retrieves context within the run's scope and context budget, and the run records what was supplied and where it came from.

## Product page, bounded tasks (held until the dod ships)

A definition of done is an optional control for work with an endpoint. This section never leads the page.

### Define done before the work starts

A dod is a short file with four kinds of check: a command that must exit clean, a file that must contain a line, a diff that must stay inside a set of paths, and a judgment a named person signs after the run. You write one by hand for a task that repeats, or Oxagen drafts one from your prompt. Either way it is locked before the first tool call, and the agent can read it and cannot change it.

### The agent doesn't get to decide it's done

When the agent tries to stop, the wrapper runs every check on the agent's own machine and records the results as a frame in the run. Broken, and the agent gets the failing ids and keeps working. Held, and Oxagen settles the result. Broken three times, and the run ends broken, on the record, with the reasons.

### Another person can recompute the verdict

The verdict is a pure function of the run's frames. There is no model in it. Export the run and hand it to your auditor. `oxagen dod verify` recomputes the verdict with no account and no network and shows whether it matches. A held verdict means the specified checks held. Your team decides whether those checks were enough.

## Docs, writing a rule

Write one rule for each thing an agent may ask for. A rule names a capability, a condition, and one of three effects: allow, deny, or require approval. Rules apply to requests routed through Oxagen. Requests that match allow or deny settle inside the call and leave a row with the rule id. Requests that match require approval wait for the person the rule names, and the run waits with them. When no rule matches, the request is answered by identity alone. Rules are checked after identity, so a rule can narrow what an agent may do and never widen it.

## Docs introduction

Oxagen is Mission Control for an autonomous agent workforce: assign identities, set authority, equip agents with tools and skills, and oversee their work through a shared control plane.

To enroll Claude Code, open the Agents page and choose Wrap Claude Code. The installer writes the wrapper beside Claude Code and enrolls this machine. The page turns to connected when the first frame arrives. From then on, actions routed through Oxagen are checked against the agent's mandate and recorded on the run page. In observe mode they are recorded, not enforced.

For a bounded task with a definition of done, the wrapper also locks the dod when you submit a prompt and runs its checks when the agent tries to stop. Setup time and hook counts appear here only with a dated measurement.

## Launch announcement

An autonomous agent workforce needs operators who can decide what each agent may do, equip it for the work, and inspect what happens. Oxagen is Mission Control for that workforce.

Give each agent an identity. Set its authority and budget, assign tools and skills, and review requests that need a person's decision. The record connects governed actions to the agent, the rule, and the person responsible.

Some work has a finish line. Define its completion criteria before the agent starts and review the recorded results afterward. Other work continues. Keep its authority, activity, and spend in view as it runs.

Explore Mission Control to follow an agent from its assigned mandate to a recorded action.

## Sales email, operators

Subject: Who sets the authority for your agents?

When your team adds an autonomous agent, where do you set its identity, authority, tools, skills, and budget? Where do you review the actions it takes?

Oxagen brings those decisions into Mission Control. A walkthrough follows one agent from its mandate through an approval request and the resulting record. It also shows how an operator defines completion for a bounded task and manages an ongoing responsibility.

Explore Mission Control.

## Sales email, security

Subject: Which identity and rule authorized that agent action?

When an agent requests an action, can your team connect it to the agent's identity, the authority it was assigned, and the person responsible?

Oxagen applies your rules to requests handled through the platform and records the decision. A walkthrough shows the agent's mandate, a denied request, one routed to a person, and the record each leaves.

Inspect an agent's authority.

## UI strings

| Place | String |
|---|---|
| Access page, request row, allowed | allowed by rule `repo-read` |
| Access page, request row, denied | denied by rule `no-push-main` |
| Access page, request row, routed | waiting on Priya, rule `release-branch` |
| Request card header | Access request |
| Request card body | Push to `release/2026.09` requested by stella for Dana's task. Reaches 14 files in `apps/api`. |
| Request card actions | Allow for this run · Deny |
| Empty state, no rules | This workspace has no rules. Requests are answered by identity alone. Write the first rule under Access. |
| Connection row | GitHub, mediated connection. Held by the workspace. The agent does not receive the credential. |
| Enforcement, observe mode | recorded, not enforced |
| Fleet page, agent outside the boundary | Not enrolled. Oxagen does not record or govern this agent's calls. |
| Run page, dod panel header (bounded tasks) | Definition of done |
| Lock line (bounded tasks) | Locked before tool call 1, `sha256:4c1f…9e02` |
| Verdict, held | held |
| Verdict, pending | pending, waiting on reviewer |
| Verdict, broken | broken: unit, scope |
| Stop blocked toast in Claude Code | Stop blocked. Broken: unit, scope. |
| Empty state, no dod | This run has no definition of done. That is fine for ongoing work. For a bounded task, add one under `.oxagen/dod/` or let Oxagen draft it at the next prompt. |
| Verify success | Verdict matches. held, 0 reasons, seal ok. |
| Verify failure | Verdict does not match. Expected held, found broken. See the diff below. |

## Error messages

Say what happened, then what to do. Never apologize. Never say "oops."

- Request denied by rule `no-push-main`. Push to a branch and open a pull request.
- Request is waiting on Priya, rule `release-branch`. The run resumes when she answers.
- This run is in observe mode. Its calls were recorded, not enforced. Wrap the harness to enforce the mandate on calls routed through Oxagen.

For bounded tasks with a dod:

- The dod on disk does not match the lock. Restore `$OXAGEN_RUN_DIR/dod.toml` or start a new run.
- Check `unit` timed out after 600 seconds. Raise `timeout_s` in the dod or split the command.
- No dod matched task `github:macanderson/oxagen#2701`. Oxagen drafted one. Review it at `$OXAGEN_RUN_DIR/dod.toml`.

## Ad directions

Candidates for rendering from the message registry. They are not additional approved taglines or measured winners. The primary ad introduces the control plane. The others explain one operator decision each.

**Primary.**
Headline: Mission Control for your autonomous agents.
Support: Set identity, authority, tools, and skills. Oversee the work.
Action: Explore Mission Control

**Authority.**
Headline: Which agent can do what?
Support: Set authority and review the decisions behind governed actions.
Action: Inspect agent authority

**Equipment.**
Headline: Equip each agent for its work.
Support: Manage tools and skills through the agent's mandate.
Action: Explore agent equipment

**Completion.** Held until the dod ships.
Headline: Define the finish line before work starts.
Support: For bounded tasks, set completion checks and review their results.
Action: See a checked run

**Finance.**
Headline: See which agent spent what.
Support: Inspect recorded costs by person, agent, and run.
Action: Inspect workforce spend

## Oxagen and Stella

Candidate line, pending confirmation of the launch integration: Stella is an agent. Oxagen is Mission Control for the workforce.

Show Stella as one supported agent where that is accurate. Keep the platform story broad enough for other supported agents. Shared branding does not establish shared proof mechanisms or identical capabilities.

## Commit messages and PR descriptions

Same voice, present tense, what changed and why in one line each.

- `dod: lock before the first tool frame, LOCKED_LATE otherwise`
- `dod-harness: one dod.checked frame per stop instead of one per check`
- PR description: "Adds the settle handler. It reads the chain, runs decide(), and appends dod.settled. Human checks resolve through the existing approvals; there is no dod.sign."
