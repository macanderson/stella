---
name: quality-gates
description: >
  Shared definition-of-done for shipping features: the five mandatory UI
  states, stability gates, performance budgets, accessibility floor, and test
  requirements. Use when building (feature-shipper, /ship-feature) or
  reviewing (code-reviewer, usability-reviewer) any user-facing feature.
---

# Quality Gates — Definition of Done

## The five UI states (all designed and built, no exceptions)

1. **Loading** — skeletons that match final layout (not spinners, no layout
   shift).
2. **Empty** — teaches what the capability is for and offers the fastest path
   in (including the AI-assisted one where it exists).
3. **Error** — one designed treatment per distinct API error code; message
   says what happened and the way out. Collapsing all failures into
   "Something went wrong" is a defect.
4. **Partial** — degraded data or mixed success rendered honestly.
5. **Ideal** — the happy path.

Plus **permission-denied**: contextual explanation of the required role/scope
and the path to request it — never a bare 403.

## Stability gates

- Every network call: timeout, bounded retry with backoff + jitter where
  idempotent, and a designed failure rendering. No swallowed promises or
  empty catch blocks.
- Optimistic updates only with rollback + conflict handling; otherwise
  pessimistic with honest progress.
- Error boundaries isolate the feature — its crash never takes down the shell.
- Long-running operations survive refresh/navigation (server-held, resumable
  state).
- Feature-flagged; safe to ship dark; kill switch documented.

## Performance budget (state actuals vs budget in the PR)

- Defaults unless overridden: LCP < 2.0s, INP < 200ms, route JS < 200KB gz,
  p95 API < 300ms.
- Code-split the route; no new dependency > 30KB gz without written
  justification and a lighter-alternative check.
- Lists that can exceed ~100 rows: virtualized + server pagination; fetch only
  rendered fields.
- Cache reads with a stated staleness tolerance; dedupe in-flight requests.

## Accessibility floor

Keyboard-complete (every action reachable and operable), visible focus states,
focus trapped in modals and returned on close, semantic HTML + labeled inputs,
WCAG AA contrast, prefers-reduced-motion respected, async status changes
announced to screen readers.

## Test gates

Unit tests for domain logic; integration tests for the API contract including
error codes; component tests covering all five states; one e2e for the
critical happy path AND one for the primary failure path. Full suite green;
lint, typecheck, a11y lint pass.

## Ship checklist

- [ ] Five states + permission-denied built and screenshotted
- [ ] Stability gates met (timeouts/retries/boundaries verified)
- [ ] Budget-vs-actual performance numbers attached
- [ ] Accessibility floor verified by keyboard-only walkthrough
- [ ] Tests per test gates, suite green
- [ ] Command-palette + empty-state registration per surfacing spec
- [ ] Flag name + rollout plan documented
- [ ] Reflection written per reflective-memory skill
