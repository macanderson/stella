---
name: ai-assisted-config
description: >
  The canonical 8-step pattern for AI-assisted configuration UIs. Use whenever
  building or reviewing any surface where an AI drafts structured configuration
  (agent policies, RBAC rules, capability contracts, routing rules, metering
  plans) — high-dimensional config where the user knows their INTENT but not
  the SCHEMA. Also defines the hard boundaries the AI must never cross.
---

# AI-Assisted Configuration — Canonical Pattern

Build it exactly like this, every time. The form is the source of truth; the
AI is an accelerator bolted onto it, never a bypass around it.

1. **Intent capture** — natural-language input plus context the system already
   knows (org, environment, selected resources). Offer 2–3 example prompts in
   the empty state.
2. **AI drafts** a structured config against the real schema.
3. **Render the draft into the form** — never apply opaque AI output. The
   draft populates the same inspectable, editable form/JSON the manual path
   uses.
4. **Provenance per field** — each AI-set value gets a "why this value"
   affordance; low-confidence values are visually marked and listed first for
   review.
5. **Deterministic validation is the authority** — schema + policy + RBAC
   validation runs on the draft exactly as on hand-entered input. The AI can
   never bypass, weaken, or "explain away" a validation failure.
6. **Diff & blast-radius preview** before apply: what changes vs current
   config and what it affects ("grants 3 agents write access to prod
   billing"). Mandatory for governance objects.
7. **Apply = versioned + revertible** — every applied config is a version
   with one-click rollback. Log the prompt, the draft, the human edits, and
   the applier for audit.
8. **Escape hatches both ways** — switch to fully manual editing at any moment
   without losing state; from manual mode, invoke AI to explain existing
   config or propose a diff ("make this policy read-only for contractors").

## Hard boundaries

- The AI never invents secrets, credentials, external identifiers, or dollar
  amounts — those fields are always human-entered and visibly marked as such.
- For security- and billing-critical objects, the apply step defaults to a
  human review checkpoint even in mostly-autonomous flows.

## Test requirements for any implementation

- Invalid AI drafts are rejected by validation (prove it with a test).
- Provenance renders for AI-set fields.
- Rollback restores the prior version exactly.
- The escape hatch to manual preserves all state.
