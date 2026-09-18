# Positioning

Read this before writing any headline, hero, tagline, ad, or opening sentence.

## The claim

An autonomous agent workforce needs operators who can decide what each agent may do, equip it for the work, and inspect what happens. Oxagen is Mission Control for that workforce.

Other layers each cover one part of the operator's job. Identity systems say who the agent is. Gateways say which tools it can call. Billing says what it consumed. Prompt repos say what it was told. Oxagen binds those decisions into one mandate per agent, applies the mandate to the actions routed through Oxagen, and keeps a record another person can read.

Every control claim carries its scope. Say "for actions routed through Oxagen" or "on governed calls". An agent outside that boundary, or a call that does not pass through Oxagen, is not governed by it. Observe mode is recorded, not enforced. Detail pages state the enrollment boundary, the supported integrations, and the default when no rule matches.

## The category

**Mission Control** is the product and the experience. It is where the operator does the job: give each agent an identity, set its authority and budget, equip it with tools and skills, answer its requests, and review its activity and spend.

**The agent control plane** is the technical category. It is what Oxagen is to the systems it governs. A control plane does not run the workload. It decides what the workload may do, hands it what it needs, and keeps the record. Oxagen does not run agents. Agents run in their supported environments.

Use Mission Control in headlines and anywhere the reader will operate the product. Use agent control plane when the reader needs the technical category: an architecture review, a platform owner's evaluation, a docs overview. One product, one category. Do not claim a second category name.

**Fleet** is the name of the Fleet page and part of the operator's vocabulary. Fleet management describes the operator's job of seeing enrolled agents and acting on them. It is not a second category. Spend management is part of the same job: the budget, the meter, the rules, and the bill per agent, run, and person sit beside the agent, not in a separate finance tool.

**Workforce** is the population of autonomous agents an organization runs. "Autonomous agents" is fine as a plain description of what Oxagen manages.

Do not name the category as observability, governance, evals, guardrails, a trust layer, or orchestration. Those words describe other products. Describe Oxagen by the operator's job and the mechanism a buyer can inspect.

## The mandate

Each agent has its own identity. The unit Oxagen manages for that identity is the mandate. Security, FinOps, and engineering each write one clause, and the platform keeps the fourth. These can be one person or several teams. The copy does not require a particular org chart.

| Clause | Who sets it | What it says |
|---|---|---|
| Access | Security | The identity the agent acts as, which systems it may request, which data and graph scope it may read, which actions it may request |
| Budget and rules | FinOps | What it may spend, under which commercial terms, and the rules it must obey: approval thresholds, allowed vendors, decision rules |
| Equipment | Engineering | The tools and skills it may use, the business context it is permitted to read, and the steering it runs under |
| Record | The platform | For governed activity: what the run read, what it changed, what it cost, which rule answered each request, and, for bounded tasks, which checks held |

Lead with the clause the reader owns. A security lead reads identity and access first. A finance lead reads budget first. An engineering lead reads equipment first, then the record. The operator reads all four.

Define "mandate" once, beside the task it names: "Set an agent's authority, budget, tools, and skills in its mandate." Do not open with the word before the reader knows what it holds.

Detail pages on identity name the supported identity systems and explain provisioning, delegation, and revocation. "Its own identity" describes the implemented relationship, not only an internal agent id.

## Completion is optional

Some work has an endpoint. Other work continues. Both belong in Mission Control.

- **Bounded tasks.** Define completion before the agent starts: the checks and human decisions that decide it. Oxagen locks the criteria into the run and records the results. When checks fail, the record shows the reasons and whether the agent continued, waited for a person, or reached its retry limit.
- **Ongoing responsibilities.** Manage them through authority, budget, and review points. Do not invent a finish line for work that has none.
- **What a verdict means.** A passing verdict means the specified checks held. Your team decides whether those checks are enough for the task. It does not establish that every requirement was captured or that every action was correct.
- **Proven.** Use it only beside a witness flip and its scope. Never for anything a dod did alone.

The dod lines are held until the dod ships. When it ships, they apply to bounded tasks and never lead a page.

## The keys stay with you

Don't hand your agents the keys. This is the sentence under the access clause, and it shapes every line about identity, connections, credentials, permissions, and tools.

The familiar model is a service account: an agent is given a token with everything it might ever need, and the team hopes it uses only some of it. Oxagen's model is an identity and a request. Each agent has its own identity and a mandate. When a task needs a system, a scope, or an action through a mediated connection, the agent asks for it at the moment of use. Each request names who started the task, which agent is asking, which tool it wants, and which data it would reach.

A rule answers the request. The team that owns the system writes the rule, and a rule has three answers: allow, deny, or route it to a named person. Allowed and denied requests settle inside the same call and leave a row. Routed requests wait for the person, and the run waits with them. For mediated connections, Oxagen uses the connection credential on the agent's behalf, and the agent does not receive it. The agent's own identity and authentication are separate from that connection credential. The answer, the rule that gave it, and the person who signed it are in the record.

Write it this way on every surface:

- **The agent asks.** Never "the agent has access to Slack" or "connect your agent to Slack". Say "the agent can request Slack" or "Slack is in the agent's mandate".
- **The rule decides.** Name the three answers when there is room: allowed, denied, routed to a person. A rule that always allows is still a rule someone wrote and can read. The scope goes with it: rules answer requests routed through Oxagen. A detail page says what happens when no rule matches.
- **The record keeps the answer.** Every governed request is a row with the rule that answered it and, when routed, the name of the person who did.
- **For mediated connections, the credential stays in Oxagen.** Say Oxagen uses the connection credential on the agent's behalf and the agent does not receive it. Never "securely stores your keys". Never promise that the agent has nothing to leak. The agent still holds its own identity and whatever reaches its context.
- **No fear.** Do not tell the reader their setup is dangerous, and do not assume what their setup is. Tell them what the request looks like and who answers it.

Proof points, stated so they survive a rebuttal:

- **Three answers, one rule.** A decision rule names a capability, a condition, and one effect: allow, deny, or require approval. For governed calls, the kernel checks it after identity and entitlement and before the handler runs, so a denied action does not reach the code that would have done it.
- **Routing is built in.** Any governed action can be marked as needing a person. The run pauses on the request, the person answers from the Access page, and the run resumes with the answer in the record.
- **For mediated connections, the credential is Oxagen's to hold.** Connections are held by the workspace, encrypted under a key you own, and used inside the platform on the agent's behalf.
- **Every governed request is a row.** Who started the task, which agent asked, which tool it wanted, what data it would reach, which rule answered, and who signed. The same row the meter prices.
- **The default is stated.** When a workspace has no rules, requests are answered by identity alone. The Access page says so.

## The lead lines

**Decision, 2026-09-15, messaging review:** Mission Control is the lead, agent control plane is the category, and completion is an optional control for bounded tasks. The bill, waste, memory, proof, and fleet lines are retired for the reasons in the table below. The dod lines stay held until the dod ships.

The approved lines live in the message registry at `messages/` in `oxagenai/oxagen-brand`. Each entry carries its audience, long and short copy, release status, evidence, owner, and review date, and the registry generates `message-bank.html` and the ad copy. When this list and the registry disagree, the registry wins. The list below is here so the vendored copy of this skill works without the registry. It carries no count, so adding or retiring an entry never leaves a stale number behind.

### Live

| Line | Use it for |
|---|---|
| **The control plane for your agent workforce** | The homepage eyebrow |
| **Mission Control for your autonomous agents.** | The homepage headline, the primary ad, decks |
| **Oxagen is Mission Control for an autonomous agent workforce: assign identities, set authority, equip agents with tools and skills, and oversee their work through a shared control plane.** | The one-sentence definition, the first line of an overview, the docs introduction |
| **Don't hand your agents the keys.** | The access section of the site, security outreach |
| **Identity, authority, and a record you can inspect.** | The security page headline |
| **See which agent spent what, and on whose behalf.** | The finance entry point, the Spend page, finance outreach |
| **Give agents the business context their work requires.** | The knowledge entry point |
| **Set the authority behind each action.** | The authority section |
| **Give each agent the tools and skills its work requires.** | The equipment section |
| **Define completion when the work has an endpoint.** | The work section |
| **Manage the workforce from Mission Control.** | The operations section, the Fleet page intro |
| **Follow the action back to its authority.** | The audit section |

Candidate, pending confirmation of the launch integration: **Stella is an agent. Oxagen is Mission Control for the workforce.** Show Stella as one supported agent where that is accurate. Shared branding does not establish shared proof mechanisms or identical capabilities.

### Held until the dod ships

| Line | Use it for |
|---|---|
| **The agent doesn't get to decide it's done.** | The bounded-task section only. Never the lead. |
| **Define done before the work starts.** | Locked completion criteria, bounded-task docs |

Earlier held lines that are not carried into the registry: "Define done before the agent starts. Prove it after." (pairs proof with every run, finding 01), "Runs that end with a verdict, not a claim.", "Prove it to someone who doesn't trust you.", and "Steer. Govern. Observe." (names owned category words). Do not use them unless a registry entry restores them.

### Retired

| Line | Finding | Why | Use instead |
|---|---|---|---|
| Can you explain your AI bill? Neither can your provider. | 06 | Makes an unsupported comparison with every provider and argues about providers instead of attribution | See which agent spent what, and on whose behalf. |
| Stop wasting money on AI. | 02 | Promises a lower bill without a measured workload or conditions | See which agent spent what, and on whose behalf. |
| Never re-explain yourself to AI ever again. | 05 | Promises universal recall and breaks the rule against outcome absolutes | Give agents the business context their work requires. |
| Agents that prove their work. A model you own. | 01, 02 | Makes proof the defining feature and implies every run is proven | Mission Control for your autonomous agents. |
| Run your agents as a fleet. | 07 | Competes with Mission Control as a second category name | Manage the workforce from Mission Control. |
| Oxagen learns from every run, and the next one costs less. (subline) | 02 | Promises that every next run costs less, whatever the task or model | See what each recorded run costs. |
| Teach Oxagen your business once. Every agent you run has it. (subline) | 05 | Implies universal distribution while the mandate restricts scope | Give agents the business context their work requires. |

Do not write new taglines. If a surface needs a line, pick a live one, or add an entry to the registry with its evidence and owner.

## The pitch, one sentence

Oxagen is Mission Control for an autonomous agent workforce: assign identities, set authority, equip agents with tools and skills, and oversee their work through a shared control plane.

## The pitch, short

Your operators decide what each agent is responsible for, what it may do, and what it needs to work. Oxagen gives those decisions a home: an agent identity, a mandate for authority and budget, assigned tools and skills, and a record of governed activity. For bounded work, define completion before it starts. For ongoing work, manage the agent's authority, requests, and spend as it operates.

## Proof points, stated so they survive a rebuttal

- **One mandate, checked at the call.** Identity, knowledge scope, permitted action, commercial terms, and the audit record are one typed contract. For actions routed through Oxagen, it is checked when the agent calls, not reconstructed after the incident.
- **The meter is a row, not an estimate.** Every governed action is priced and attributed to the person, the agent, the run, the turn, and the step. The Spend page and the record page read the same rows. Measured, reported, and estimated costs are labelled as such.
- **Your keys, your graph, your model.** Own model keys, own graph endpoint, hosting at cost.
- **Whoever built the agent.** Claude Code, the Agent SDK, or a custom loop, through a supported wrapper. The wrapper records and gates from beside the agent. Oxagen does not run it. Name the supported integrations on the detail page.
- **The workforce on one page.** Every agent enrolled in Oxagen, whoever built it, is on the Fleet page with its mandate, its open requests, its spend, and its last run.
- **Spend beside the agent.** The budget an agent runs under, what it has spent this month, and the rule that stops it sit beside the agent, priced per governed action. The finance lead and the operator read the same rows.
- **Equipment is assigned, not assumed.** Tools and skills reach an agent through its mandate. A tool in the catalog does not grant every agent permission to use it. Each request still meets the decision rules.

Held with the dod lines, for bounded tasks only:

- **Pre-committed, not post-hoc.** The dod is locked before the agent moves, and its position in the run's chain shows it.
- **Two halves, on purpose.** The dod is visible to the agent, so drift is caught where it happens. The witness is hidden from the agent, so gaming is caught where it hides.
- **No model in the verdict.** `decide()` is a pure function of the run's frames.
- **Another person can recompute the verdict.** `oxagen dod verify` runs with no account and no network. It shows whether the recorded verdict matches, not that every requirement was captured.
- **The meter follows the proof.** Charge for proven runs. Report runs and governed actions as secondary meters.

## The buyer we are built for

The operator is the user. The buyer is the technology executive who answers for the agent workforce and its spend, usually a CIO or a CTO, or the platform leader who answers for both. Operators adopt Mission Control because it is where their job happens. The buyer signs because they have to answer for what the workforce did.

### The statement

Oxagen sells to the technology executive who is asked to ship autonomous agents across the company and to account for what those agents do, at the same time, with a modernization program already underway. They can see the total. They cannot yet say which agent spent it, on whose behalf, under which rule, or why a task took the path it took. Leadership above them is asking for more output from a smaller team, and the answers available today are estimates. What they buy from Oxagen is the ability to answer: an identity per agent, a mandate a named team wrote, and a record of governed activity that another person can read.

### What qualifies them

- Agents run in more than one team, built by more than one group, and no single page lists them.
- A named person is accountable for agent spend and reports a number upward that they cannot attribute below the account.
- A migration or modernization program is in flight, so new agent work lands beside systems that already have owners, change windows, and auditors.
- Security, finance, and engineering each own part of what an agent may do, and each of them writes their part somewhere different today.
- Someone above them has asked, in the last quarter, for output the current reporting cannot substantiate.

### What sets them aside, for now

- One team, one agent, one person who holds the whole picture.
- Agents that only read. The mandate earns its keep when an agent requests actions in systems other people own.
- No one accountable for agent spend, which means no one to read the Spend page.
- A mandate to stop agent adoption rather than to operate it. Mission Control is for running the workforce.

### How to open with them

Ask. Do not assert. `rule-outreach` governs first contact and it applies here: do not tell this buyer what their agents hold, what their reporting hides, or how their teams work. Open with one of these:

- When a team adds an agent, where does its authority get set, and who sets it?
- Can you attribute last month's agent spend to the agent, the run, and the person who started the run?
- When an agent requests an action in a system another team owns, which rule answers it, and where is that answer recorded?
- Who signs when the answer is a person?

### The pain stays internal

What this buyer says in a room is qualification material, not copy. A large invoice for a small change, a review that cannot name the agent or explain why it took the path it took, and pressure from above for output the reporting cannot substantiate: all three sharpen discovery and none of them belongs in a headline, an ad, a subject line, or a slide. Published copy states the mechanism and lets the reader draw the comparison. The registry carries this as `rule-icp` and `rule-buyer-pain-internal`, and finding 06 is why the bill headline was retired.

## The buyer and the sentence they repeat

Hypothetical. These are the sentences the copy should make true. They are not testimonials, and they never appear in customer-facing copy as quotes. A real quote needs the customer's attribution permission.

- The technology executive, asked in a board meeting: "I can say which agents did the work, what each one was allowed to do, and what it cost."
- The operator, after a week: "I can see what each agent is waiting on, and answer it from one place."
- The security lead, after a month: "For the connections Oxagen mediates, the agents ask, and I can read every answer."
- The finance lead, reading the Spend page without translation: "I can say which agent spent what, on whose behalf, under which rule."
- The engineering lead: "I set each agent's tools and skills in its mandate, and I can see which ones reached the work."

The primary audience is the operators and platform teams accountable for an autonomous agent workforce. Security, finance, and engineering get their own entry points into the same control plane. Validate this language through interviews before treating any of it as the buyer's own words.

## The demo, in order

1. Open an agent and show its identity and the operator accountable for it.
2. Show the mandate it works under: its authority, its budget, and its rules.
3. Assign a tool and a skill through the mandate.
4. Let the agent request an action a rule allows, and show the row with the rule id.
5. Let it request an action that needs a person. Answer the approval request from the Access page and watch the run resume.
6. Open the record: the request, the rule that answered it, the person who signed, and the cost beside the tool call.
7. Show an ongoing agent: its authority, its open requests, and its spend this month, with no finish line.
8. Show a bounded task with completion criteria: the locked checks and their recorded results.

A denied request fits between steps 4 and 5 when the reader is a security lead. Describe what each control changes and when it takes effect. Do not promise the demo on a prospect's own systems without agreed setup.

## Competitive framing

Never name a competitor in copy. Do not dismiss a category either: no lines about what observe tools, governance tools, or evaluation tools fail to do. Show the mechanism instead: an agent's identity, the rule that answered a request, the person who approved it, and the recorded cost. The reader draws the comparison.

## What not to say, and why

- **Trust layer, safety, guardrails.** These sell fear. The buyer is not afraid; they are annoyed.
- **AI-powered verification.** The verdict has no AI in it. That is the point.
- **Connect your agent to, give the agent access to, the agent's API key.** Each one hands over the keys in the copy even when the product does not. The agent asks; the rule answers.
- **Auto-approve, as a feature.** A rule that allows is not automation; it is a decision someone wrote down. Say "allowed by rule".
- **Enforced on every call, on every run, with no scope.** Say "for actions routed through Oxagen" or "on governed calls". Observe mode is recorded, not enforced.
- **Nothing to leak, or any similar leak promise.** Credential custody for mediated connections is the claim. The agent still has its own identity and its own context.
- **SOC 2 compliant, SOC 2 certified, or any attestation claim without the report.** Publication rule: state an actual completed examination, report type, covered entity and system, examination period or date, and report-access process only when the report supports each one. If the status is readiness work or support for customer controls, describe that specific status and scope. Do not turn "auditable" or an ambition into an attestation claim.
- **Savings without measurement.** "Costs less", "fewer tokens, same answers", or "stop wasting money" need the measured workload, baseline, model and rate versions, sample size, quality measure, and costs including retries and Oxagen fees. Without them, the claim is cost visibility and attribution.
- **Comparisons with providers.** Do not say what a provider's reports cannot do. Show the attribution row.
- **Universal memory or recall.** Not "every agent you run has it", not "never re-explain". Context is given within the agent's permitted scope, and storing context does not guarantee accurate recall.
- **Asserting a prospect's setup.** Do not tell a prospect what token their agents hold or who reads their PRs. Ask a diagnostic question.
- **Traction numbers without dated evidence.** Partner counts, customer counts, team size, and setup durations such as "sixty seconds" stay out of copy until an owner provides dated evidence and publication approval.
- **Completion as the defining feature.** The dod is an optional control for bounded tasks. It never leads a page, and ongoing work needs no finish line.
- **Two category names, or owning a category.** Mission Control is the product. Agent control plane is the category. Fleet is a page name.
- **The fourteen oracles, the ladder, the rating, Vera.** Year-two story. Telling it now is the surface-area problem.
- **Stamp.** Belongs to the witness. The dod settles; it does not stamp.
- **Proven, for anything the dod did.** A held dod means the specified checks held. Proven is the witness verdict `flipped`, stated beside its scope, and nothing else.
