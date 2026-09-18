# Voice and tone

## The voice, in one line

A senior engineer who has read the logs and is telling you what happened.

## Five traits

**Plain.** Short words, short sentences, no jargon that a second-year engineer would have to look up. When a technical word is the right word (frame, seal, digest), use it on the detail page that defines it, and do not apologize.

**Specific.** Numbers, names, and file paths where a competitor would put an adjective. "Blocked once, held on the second stop" instead of "reliable enforcement." A number is measured and dated, or it stays out.

**Unhurried.** No exclamation points. No urgency. Nothing is "finally here." The product does not need the reader to feel anything to understand it.

**Dry.** Understatement over emphasis. If a sentence would be stronger with the intensifier removed, remove it. Humor is allowed only as understatement and never as a joke.

**Honest to the record.** Say exactly what the evidence supports and stop. A control applies to actions routed through Oxagen, and the copy says so. An observe-mode run is recorded, not enforced. A held dod means the specified checks held, not that the work is correct. Proven is the witness's word, and only beside a witness flip and its scope. The UI never shows a stronger word than the frame allows, and neither does the copy.

## Tone by surface

| Surface | Tone shift | Example |
|---|---|---|
| Website hero | Most compressed. One claim, one sentence of how, one action. | Mission Control for your autonomous agents. Give each agent an identity. Set its authority, budget, tools, and skills. Explore Mission Control. |
| Product page | Explains the mechanism in order. Reads like a good README. States the scope beside each control. | see `examples.md` |
| Docs | Second person, imperative, one step per sentence. | Open the Agents page and choose Wrap Claude Code. Run a prompt. The page turns to connected when the first frame arrives. |
| UI strings | Terse, present tense, never a full sentence where a fragment reads faster. | Denied by rule `no-push-main`. Waiting on Priya, rule `release-branch`. recorded, not enforced. |
| Access requests | The request first, then the answer, then who gave it. Never the credential. | Push to `release/2026.09` requested by stella for Dana's task. Waiting on Priya, rule `release-branch`. |
| Errors | Say what happened and what to do, in that order. Never apologize. | Request denied by rule `no-push-main`. Push to a branch and open a pull request. |
| Sales email | Two short paragraphs. The first asks a diagnostic question. The second names one thing a walkthrough would show. Never assert the prospect's setup. | see `examples.md` |
| Launch post | Same as the product page, plus one paragraph on why now. No "we're thrilled." | see `examples.md` |
| Investor | Facts in sequence, each number dated and sourced. Never adjectives about the team. | Not kept in this skill. The repo is public. |

## Before and after

Each pair shows the same idea in the wrong voice and then in Oxagen's. No "After" line may break a rule in `positioning.md` under *What not to say*.

**Enthusiasm**
Before: We're excited to introduce a revolutionary new way to manage your AI agents!
After: Mission Control for your autonomous agents.

**Abstraction**
Before: Oxagen provides comprehensive governance and observability for autonomous agents across your organization.
After: Give each agent an identity, then set its authority, budget, tools, and skills in one mandate.

**Unscoped enforcement**
Before: The mandate is enforced on every call.
After: For actions routed through Oxagen, the mandate is checked at the call. Observe mode records the call without enforcing it.

**Overclaiming, a bounded task**
Before: Every run is verified and proven correct.
After: A passing verdict means the specified checks held. Your team decides whether those checks are enough. Proven is the witness's word, and it applies only when a witness flipped.

**Completion as the lead**
Before: The agent doesn't get to decide it's done. (as a homepage headline)
After: Define completion when the work has an endpoint. (The dod line is held until the dod ships, then used in the bounded-task section only.)

**Fear**
Before: Unchecked agents are a liability waiting to happen. Protect your organization today.
After: When an agent requests an action, the record shows which rule answered and who approved it.

**Asserting the prospect's setup**
Before: Your coding agents run with a GitHub token that can push to every repo.
After: When an agent requests a repository action, can your team see which rule allowed it and who approved it?

**Passive voice**
Before: Requests are approved by the release owner before the push is made.
After: The release owner approves the request, and then Oxagen makes the push.

**Passive voice, a bounded task**
Before: The definition of done is locked before tool execution begins.
After: The wrapper locks the dod before the agent's first tool call.

**Adjective instead of number**
Before: Fast, seamless setup.
After: Open the Agents page and choose Wrap Claude Code. The page turns to connected when the first frame arrives. (A setup time goes here only with a dated measurement and the environment it was measured on.)

**Jargon for its own sake**
Before: Cryptographically attested, tamper-evident evidence chains with Merkle-rooted seals.
After: Every frame is hash-chained to the one before it, and the seal signs the close of the run. A reviewer with the export can check its integrity outside the product.

**Marketing "we"**
Before: We believe agents should be accountable.
After: Each agent works under a mandate the accountable teams can read and change.

**Handing over the keys**
Before: Connect your agent to GitHub, Slack, and your database in one click.
After: The agent asks for GitHub when the task needs it. A rule you wrote answers, or a person you named does. For this mediated connection, the credential stays in Oxagen.

**Automation as the sell**
Before: Requests are auto-approved so your agents never wait.
After: Reads of the public repo are allowed by rule. A push to main is denied by rule. A push to a release branch waits on the release owner.

**Vault talk**
Before: Enterprise-grade secret management keeps your credentials safe.
After: Connections are held by the workspace and encrypted under a key you own.

**Leak promise**
Before: The agent never sees the key, so there is nothing for it to leak.
After: For mediated connections, Oxagen uses the connection credential on the agent's behalf. The agent does not receive it, and the record shows each use.

**Unmeasured savings**
Before: Oxagen learns from every run, and the next one costs less.
After: See what each recorded run costs, attributed to the person, the agent, and the run.

**Provider comparison**
Before: Can you explain your AI bill? Neither can your provider.
After: See which agent spent what, and on whose behalf.

**Universal context**
Before: Teach Oxagen your business once. Every agent you run has it.
After: Give agents the business context their work requires, within the scope their mandate permits.

**Attestation by implication**
Before: Fully auditable, so you are SOC 2 ready out of the box.
After: Review how agent identities map to your access controls and what each governed action records. (A SOC 2 statement names the actual report status and scope.)

## Punctuation and typography

- Periods and commas do the work. Colons for lists and definitions. No em dashes, no en dashes as separators, no semicolons in customer-facing copy.
- No exclamation points.
- Sentence case for headings, buttons, labels, and table headers.
- Code, commands, paths, digests, frame kinds, and verdict words in monospace: `dod.settled`, `held`, `oxagen dod verify`.
- Numbers as numerals when they are data (3 retries, 600 seconds, 7 reasons) and as words when they open a sentence.
- Oxford comma.

## Names and casing

- oxagen, stella: lowercase as wordmarks and in logos.
- Oxagen, Stella: capitalized as names in prose.
- Mission Control: capitalized, no hyphen. The product and the experience.
- agent control plane: lowercase in prose. The technical category.
- Fleet, Spend, Access: capitalized as page names. fleet, lowercase, as the operator's word for enrolled agents.
- dod: lowercase, always, as a noun. "The dod held." Never DoD, never Dod, never "the DOD file."
- Claude Code, Codex CLI: as their owners write them.
- held, pending, broken: lowercase, in monospace when used as verdict values, in plain text when used as words.
