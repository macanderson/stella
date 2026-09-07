---
id: adr/0037-a-credit-balance-lives-outside-the-engine
title: "ADR 0037: A credit balance lives outside the engine"
status: implemented
---

# ADR 0037: A credit balance lives outside the engine

- Status: accepted
- Date: 2026-09-07
- Decides: `#2891`
- Not part of the Phase 0 series.

## Context

Oxagen plans to sell support tiers and usage credits. The open issue asks this
repository to build a tier record, a ledger of credit grants and draw-downs, a
way to meter calls to a private model, and commands that print a balance.

The issue also asks a question before any of that, and asks for the answer in
writing. A user owns the disk. A user can edit any file on it. Can a file
there be the record of what a customer owes?

Stella ships under the AGPL. Anyone may read the code, change it, and build
it. A meter a user can rewrite counts nothing. A balance a user can rewrite is
a number, not a debt.

Stella also sends nothing out by default. Rule 3 in `AGENTS.md` says so. One
seat type may send a small work summary to one named address. The fields it
may send are listed in `crates/stella-store/src/content_free.rs`. A person has
to edit that list before a new field can pass the gate.

Billing is the usual reason a product grows a new way out. This one must not.

## Decision

**The seller holds the balance and the tier. The engine holds neither.**

The engine says what it did, and it can do that today. `ExecutionRollupRow` in
`crates/stella-store/src/usage.rs` carries the provider, the model, the
outcome, the token counts, and the cost of one turn.
`crates/stella-store/src/enterprise_telemetry.rs` folds that row into a closed
shape with no content in it, then queues it for delivery. Its export ledger
gives each execution one secret number, and builds the event id from it. Two
sends of one execution carry the same id, so the sink can drop the copy.

A seller can bill from that. It is the evidence side of metering, and it is
built.

## Consequences

- No money ledger lands here. Grants and draw-downs are the seller's record.
- No command prints a balance read off local disk. A command may print a
  balance the seller's service returned, and it has to name that source.
- A tier may be read from an org-managed settings file. The three-scope merge
  already lets that file beat a project file. A project file may never raise
  its own tier.
- Metered use rides the delivery path that exists. A new field on it still
  costs a hand edit to the allowed list.
- The sink counts. The engine reports.

## What this does not decide

Prices, tier names, and what one credit buys are a product call. This record
does not make it. The rest of the open issue waits on that call.
