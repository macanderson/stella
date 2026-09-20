# Words

## Use these

The product's vocabulary is Mission Control's vocabulary (spec §3). How exactly to use it depends on where the words sit.

- **UI labels, page names, and detail pages** use the exact terms below. A button says "Deny", not "Block". The Access page says "routed", not "escalated".
- **Around those terms**, familiar explanatory language is fine. "The agent asks for permission to push" can sit beside a request row that says `waiting on Priya, rule release-branch`.
- **Define "mandate" once**, beside the task it names: "Set an agent's authority, budget, tools, and skills in its mandate." Then use the word.
- **First-contact copy** (the homepage, ads, outreach, the first paragraph of an overview) uses run, request, rule, check, and cost. Those words need no definition.
- **Frames, witnesses, and verdicts** belong on detail pages, each with its definition beside it. A reader who meets "proven" without its definition will read it as a broader guarantee than it is.

| Word | Meaning | Not |
|---|---|---|
| Mission Control | the product and the experience where operators manage the autonomous agent workforce | mission control center, the dashboard, the console (as the product name) |
| agent control plane | the technical category: decides what agents may do, hands them what they need, keeps the record, and does not run them | agent fleet management (as a category), orchestration layer, trust layer |
| workforce | the autonomous agents an organization runs, managed by its operators | swarm, army |
| identity | the agent's own principal, with its roles and accountable operator, distinct from any connection credential | the agent's token, service account, API key |
| operator's job | give each agent an identity, set its authority and budget, equip it with tools and skills, and oversee its work | orchestration, agent ops, AgentOps, babysitting |
| run | one session of one agent under one operator, on one task | session, trace, execution, job |
| turn | one prompt through to the point the agent stops | round, iteration |
| step | one model call or one tool call | action event, span |
| frame | one recorded event in a run, hash-chained | log line, event, trace |
| operator | the human accountable for an agent and its runs | user, owner, initiator |
| agent | a registered principal with its own identity | bot, assistant (except in the witness's own text) |
| workspace | a governance partition inside an organization | project, team space |
| governed action | one call routed through Oxagen that it checked and recorded | invocation, transaction, call |
| mandate | the one object an agent works under: access, budget and rules, equipment, record | policy, config, profile, permission set |
| clause | one of the four parts of a mandate, owned by one team | section, setting, module |
| equipment | the tools, skills, and permitted business context assigned through a mandate | loadout, plugins (as the clause name) |
| request | an agent asking for a system, scope, or action at the moment of use | grant, token, permission (as the thing handed over) |
| rule | what answers a request: allow, deny, or route to a person | policy (as the answer), guardrail, filter, approval workflow |
| allowed, denied, routed | the three answers a rule gives | approved (fine for the person's act), blocked, escalated, flagged |
| connection | a system Oxagen reaches on the agent's behalf. For mediated connections, Oxagen holds the credential. | integration (as the noun), the agent's API key, service account |
| credential | the connection secret Oxagen holds for a mediated connection. The agent does not receive it. | key (except in the line that says not to hand it over), secret (fine in docs) |
| fleet | the Fleet page, and the operator's word for the agents enrolled in Oxagen | swarm, army, team of agents |
| fleet management | the operator's job of seeing enrolled agents and acting on them: answer, fund, hold, stop. A job, not a second category. | orchestration, agent ops, AgentOps, monitoring |
| spend management | the budget half of the operator's job: budgets, meters, rules, the bill per agent, run, and person | cost observability, FinOps tooling, chargeback |
| operate | what an operator does to agents under a mandate | run (Oxagen never runs the agent), orchestrate, drive |
| dod | the definition of done for a bounded task, as a file and as a frame. Optional. | acceptance test, spec, checklist, contract |
| check | one entry in a dod: run, file, diff, or human | test, assertion, rule |
| lock | the digest of the dod, fixed before the first tool call | hash, signature, commit |
| held, pending, broken | the three dod verdicts | passed, failed, success, error, green, red |
| done | a run whose dod is held: the specified checks held | complete, finished, successful, correct |
| settle | what Oxagen does to a dod when the run reports its stop | stamp, certify, finalize |
| verify | recompute a verdict from an export | audit, validate, confirm |
| witness | Oxagen's hidden check, run in the witness runner | hidden test, oracle (except when naming the oracle kind) |
| proven | a run whose witness verdict is `flipped`, stated with the scope of that witness | verified, validated, correct |
| wrap | install Oxagen beside a supported harness | integrate, onboard, connect |
| wrapper | the hooks or SDK adapter beside the agent | harness (the harness is Claude Code itself), plugin, agent |
| seal | the signed close of a run | finalize, commit |
| export | the file a run produces for offline verification | report, bundle, artifact |
| Spend, Run, Fleet, Access | the pages, capitalized | dashboards |

## Verbs that carry the brand

ask, request, allow, deny, route, answer, assign, equip, oversee, lock, block, hold, break, settle, verify, wrap, record, seal, decide, read, show, cost

## Avoid these

### Words that mean nothing
seamless, robust, powerful, revolutionary, cutting-edge, next-generation, game-changing, best-in-class, world-class, enterprise-grade, comprehensive, holistic, end-to-end, turnkey, frictionless, effortless, intelligent, smart, magic

### Intensifiers
very, really, truly, genuinely, incredibly, extremely, deeply, highly, super

### Emotional sells
excited, thrilled, proud, delighted, love, passionate, finally, at last, imagine

### Fear sells
liability, risk (as a scare word), exposed, unchecked, rogue, dangerous, protect, safeguard

### Category words owned by others
observability, governance (as a category name; fine as a verb and as one of the five jobs), evals, guardrails, trust layer, safety layer, AI ops, LLMOps, AgentOps, orchestration (as a category name)

### Overclaims
proven (for anything the dod did), verified (for anything a model did), guaranteed, always, never (about outcomes), 100%, zero, eliminates

### Unscoped and unmeasured claims
enforced on every call, enforced on every run (with no scope beside them), nothing to leak, nothing for the agent to leak, SOC 2 compliant, SOC 2 certified (unless the actual report says so), costs less, saves money, fewer tokens (without the measured workload and conditions), every agent has it, every agent you run (as a promise of shared knowledge), never re-explain, neither can your provider

### Wrong-vocabulary words
session, trace (as a noun for a run), attempt, execution, invocation, span, action event, re-run, render replay, stamp (dod), certificate (dod v4 has none)

### Product words we do not use
AI-powered, LLM-powered, autonomous (as a compliment for Oxagen; fine as a plain description of the agents it manages: "autonomous agents"), agentic (as an adjective for the product), copilot, assistant

### Words that hand over the keys
connect your agent to, give the agent access to, full access, the agent's API key, the agent's token, service account (for an agent), one-click connect, auto-approve (as a feature), hand over (except in the line that says not to), least privilege (say what it means: the agent asks for what the task needs, when it needs it)

### Filler that opens sentences
In today's world, As AI agents become, With the rise of, It's no secret that, We believe, We're on a mission

## Replacements for common bad lines

| Bad | Good |
|---|---|
| ensure your agents deliver | set their authority and review the recorded work |
| seamless integration with Claude Code | a supported wrapper beside Claude Code |
| AI-powered verification | a pure function of the run's frames |
| comprehensive observability | every governed action with its cost beside it |
| enterprise-grade security | for mediated connections, the agent does not receive the credential |
| enforced on every call | checked on governed calls, for actions routed through Oxagen |
| there is nothing for the agent to leak | Oxagen uses the connection credential on the agent's behalf |
| the next run costs less | see what each recorded run costs |
| stop wasting money on AI | see which agent spent what, and on whose behalf |
| can you explain your AI bill | see which agent spent what, and on whose behalf |
| every agent you run has it | give agents the business context their work requires |
| SOC 2 compliant | the exact report status, type, and scope, or the readiness status |
| your agents run with a token that can push everywhere | can your team see which rule allowed it and who approved it? |
| run your agents as a fleet | manage the workforce from Mission Control |
| agent fleet management (as the category) | Mission Control, or agent control plane |
| the agent decides it's done (as the lead) | define completion when the work has an endpoint |
| connect your agent to GitHub | the agent can request GitHub |
| the agent has access to Slack | Slack is in the agent's mandate |
| securely stores your keys | for mediated connections, the credential stays in Oxagen |
| auto-approved | allowed by rule |
| escalated to a human | routed to a person |
| least-privilege access | the agent asks for what the task needs, when it needs it |
| gain visibility into | see |
| leverage | use |
| enable you to | lets you, or cut it |
| in order to | to |
| utilize | use |
