// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The context-record plane.
//!
//! [`context_record`] holds the typed model. [`ingest`] is the boundary that
//! stamps and gates a proposal. [`records`] merges rule files and record
//! files into one order, then renders the block. [`adapt`] maps what the
//! record channel picked into steering candidates.
//!
//! No I/O. The caller passes in the clock and the file text. See README.md
//! for the boundary and for why the engine does not depend on this crate.

pub mod adapt;
pub mod context_record;
pub mod ingest;
pub mod records;
