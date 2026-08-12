// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Why [`WorkspaceInput::Steer`] exists, and why it is not a cancel.
//!
//! A doc-only module. The reasoning is long enough that carrying it as a
//! variant comment pushed [`super`] over the 1500-line guard, and it is the
//! kind of reasoning a future maintainer will want *before* deciding that Esc
//! "obviously" means stop.
//!
//! # What Esc used to do
//!
//! A single Esc cancelled the in-flight turn unconditionally. The driver then
//! truncated the partial turn out of the conversation and auto-dispatched the
//! next queued prompt as a **new** turn — "interrupt current, run next".
//!
//! On the raw step loop that is a reasonable trade. On the staged pipeline it
//! is not an interrupt at all, it is a restart: a new turn re-enters at triage
//! and context recall, so everything the cancelled turn had established — its
//! plan, its recalled frames, its approved scope, the files it had already
//! read — is derived again from nothing. A user who typed one correcting
//! sentence and pressed Esc paid for the entire turn a second time to deliver
//! it, and watched the deck's stage readout drop back to `context recall ·
//! turn $0.0000` with an approved six-step plan still reading `0/6`.
//!
//! The sentence itself did not even arrive: it stayed in the composer while
//! the turn it was written for died.
//!
//! # What Esc does now
//!
//! With anything to say — a draft in the composer, prompts in the queue, or
//! both — Esc **steers**: every waiting text goes into the running turn at its
//! next step boundary, in the order it was written, and the turn keeps going.
//! With nothing to say it is still a plain interrupt, because with no guidance
//! to deliver that is the only thing the keystroke can mean. The double-Esc
//! full stop is untouched, so "I want it to stop *now*" stays one press away.
//!
//! # Why the whole queue travels in one message
//!
//! Because it must be drained as one act. Sent as N separate inputs, a second
//! Esc arriving between two of them would steer half the backlog and cancel
//! the turn holding the rest — which is the same lost-work failure this change
//! exists to end, in a rarer costume.
//!
//! It also lets the driver take the settling decision once for all of them: a
//! turn that finished between the keystroke and the driver reading the message
//! has no boundary left to steer at, and the texts continue the thread as the
//! next turn instead of vanishing.
//!
//! [`WorkspaceInput::Steer`]: super::WorkspaceInput::Steer
