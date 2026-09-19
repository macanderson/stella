---
name: oxagen-branding
description: The authority for anything that carries Oxagen or Stella branding or speaks in Oxagen's voice. Use it whenever you create or edit a page, post, ad, email, deck, doc, spec, UI string, error message, CLI output, README, or any prose a person will read on behalf of either brand, even if the request does not say "brand" or "voice". Covers the marks, tokens, type, layout rules, positioning, one-liners, voice and tone, words to use, words to avoid, and worked examples. Oxagen and Stella share one house system; the logo is the only difference.
---

# Oxagen branding

Read this whole file first. Then read the reference for what you are making:

| Making | Read next |
|---|---|
| Anything with words in it | `references/voice.md`, then `references/words.md` |
| A headline, hero, ad, tagline, or the first sentence of anything | `references/positioning.md` |
| Anything about identity, access, credentials, connections, permissions, or tools | `references/positioning.md`, the section *The keys stay with you* |
| Outreach, a sales brief, or anything that names who Oxagen sells to | `references/positioning.md`, the section *The buyer we are built for* |
| A page, ad, deck, or UI | `references/system.md` and `assets/tokens.css` |
| Copy for a specific surface (site, ad, email, docs, UI, launch) | `references/examples.md` |

## Where this skill lives

The source of truth is `skills/oxagen-branding/` in the house brand kit, `oxagenai/oxagen-brand`, beside the marks, tokens, ads, and content cards it describes. The oxagen repo vendors a copy at `.claude/skills/oxagen-branding/` through `tools/scripts/sync-brand-assets.mjs`, and `pnpm check:brand` fails on drift. Edit the kit, run the sync, commit both. Never edit the vendored copy alone.

Copy changes start in the message registry at `messages/` in `oxagenai/oxagen-brand`. Each entry carries the line, its audience, its release status, its evidence, and its owner. The registry generates `message-bank.html` and the ad copy. Change the registry entry first, then bring this skill's inline lists into line with it.

## What Oxagen is, in one sentence

Oxagen is Mission Control for an autonomous agent workforce: assign identities, set authority, equip agents with tools and skills, and oversee their work through a shared control plane.

Every piece of copy is downstream of that sentence. **Mission Control** is the product and the experience: the place an operator does the job. **Agent control plane** is the technical category: what Oxagen is to the systems it governs. A control plane does not run the workload. It decides what the workload may do, hands it what it needs, and keeps the record. Oxagen does not run agents.

The unit Oxagen manages is the mandate. Each agent has its own identity and works under one mandate with four clauses: access (security sets it, including the identity the agent acts as), budget and rules (FinOps sets it), equipment (engineering sets it: tools, skills, and permitted knowledge), and the record (the platform keeps it). These responsibilities can belong to one person or to several teams. If a line does not connect to identity or a clause, cut the line.

Completion is optional. A definition of done is a control for bounded tasks that have an endpoint. Ongoing responsibilities are managed through authority, budget, and review points, with no artificial finish line. The dod is never the lead.

## The six rules that never bend

1. **Gold is identity, plus at most one action per screen.** Gold never carries state and never fills a surface. State is carried by shape: double border for held, dashed for pending, single for broken.
2. **No em dashes in anything a customer reads.** Use a period, a comma, or a colon. This includes UI strings, docs, ads, and specs.
3. **Sentence case headings.** Always.
4. **Wordmarks are lowercase: oxagen, stella.** In prose they are names and take a capital: Oxagen, Stella.
5. **Mission Control vocabulary is the product's vocabulary.** Run, turn, step, frame, operator, agent, workspace, governed action, mandate, request, rule. Use the exact terms for UI labels and on detail pages. Familiar explanatory language around them is fine. Never session, trace, attempt, execution, or invocation in customer-facing prose. See `references/words.md`.
6. **The agent asks, the rule decides, the record keeps the answer, for actions routed through Oxagen.** Each agent has its own identity. For mediated connections, Oxagen uses the connection credential on the agent's behalf and the agent does not receive it. A rule the owning team wrote answers each request: allowed, denied, or routed to a person. State that scope beside the claim. Never write "connect your agent to X" or "give the agent access to X", and never promise that an agent has nothing to leak. See *The keys stay with you* in `references/positioning.md`.

## Positioning, the short version

The operator's job is to give each agent an identity, set its authority and budget, equip it with tools and skills, and oversee what it does. Mission Control is where that job happens. The agent control plane is what makes those decisions apply to the actions routed through Oxagen and keeps the record of them.

Other layers each cover one part of that job. Identity systems say who the agent is. Gateways say which tools it can call. Billing says what it consumed. Prompt repos say what it was told. Oxagen binds those decisions into one mandate per agent, applies it to governed calls, and records each decision. Observe mode is recorded, not enforced. Say which one applies.

Some work has an endpoint. For those bounded tasks, define completion before the work starts. A passing verdict means the specified checks held, and the team decides whether those checks are enough. Other work continues. Keep its authority, activity, and spend in view as it runs.

The homepage eyebrow is **The control plane for your agent workforce** and the headline is **Mission Control for your autonomous agents.** For the security reader, the access line is **Don't hand your agents the keys.** For the finance reader, it is **See which agent spent what, and on whose behalf.** For knowledge, it is **Give agents the business context their work requires.** The dod lines are held until the dod ships and then apply to bounded tasks only. The full list, the retired lines, and the reasons are in `references/positioning.md`.

## Voice, the short version

Oxagen sounds like a senior engineer who has read the logs and is telling you what happened. Plain, specific, unhurried, a little dry. It states facts, names numbers, and stops. It never sells fear, never says "AI-powered", and never claims more than the record shows.

Full guidance, with before and after pairs, is in `references/voice.md`.

## Prose rules that apply everywhere

- Actor first. "The rule denies the push" not "The push is denied by the rule."
- Concrete verbs. Ask, allow, deny, route, assign, equip, lock, block, settle, verify, hold, break. Not enable, empower, leverage, ensure.
- One idea per sentence. If a sentence has a semicolon, it is two sentences.
- Numbers over adjectives, when the number is measured and dated. "Blocked once, held on the second stop" beats "reliable."
- A feature pairs with what it does for the reader in the same sentence.
- Never strengthen a claim past the evidence. A held dod means the specified checks held, not that the work is correct. Proven is the witness's word, and only beside a witness flip and its scope. An allowed request means a rule allowed it, not that the action was safe.
- Scope every control claim. "For actions routed through Oxagen" or "on governed calls", never "on every call" or "on every run" alone.
- Cut "very", "really", "seamless", "robust", "powerful", "revolutionary", and every word in `references/words.md` under avoid.
- Say Oxagen, not "we", in product copy. Say "you", never "users", when addressing the reader.

## Checklist before shipping any asset

- [ ] One gold action at most; gold nowhere else except the mark
- [ ] State shown by shape, not color
- [ ] No em dashes
- [ ] Headings in sentence case
- [ ] oxagen and stella lowercase as marks, capitalized in prose
- [ ] No forbidden vocabulary (`references/words.md`)
- [ ] First sentence connects to the Mission Control sentence and names identity or a mandate clause
- [ ] No agent holds a key, a token, or standing access anywhere in the copy; it asks, a rule answers
- [ ] Claim scope stated: control claims say "routed through Oxagen" or "governed calls", and observe mode says recorded, not enforced
- [ ] No leak promise. Credential custody is stated for mediated connections only
- [ ] No savings claim without the measured workload and conditions
- [ ] No SOC 2 attestation claim unless the actual report supports it
- [ ] Completion is presented as optional, for bounded tasks, and never as the lead
- [ ] Short forms keep the qualifiers of their longer versions
- [ ] Buyer quotes are labelled hypothetical unless an attributed customer approved them
- [ ] No traction numbers, partner counts, or setup durations without dated evidence
- [ ] Every claim is one the record can back
- [ ] Space Grotesk for wordmarks and h1 to h3 only, Geist for h4 to h6, body, and UI, Monaspace Neon for code, data, digests, and commands
- [ ] One type scale per surface: marketing (`text-m-*`) or app (`text-a-*`)
- [ ] 12px card radius, 1120px wrap, dark first on obsidian with the white light theme intact
- [ ] Gold as text on white is gold-deep `#8A7223`
- [ ] Every text role that carries meaning clears 4.5:1 on its ground
