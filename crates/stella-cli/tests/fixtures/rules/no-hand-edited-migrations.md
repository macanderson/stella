---
description: Never hand-edit an already-applied migration file.
guard-tool: Edit
guard-deny-path: migrations/*-applied/**
---

Applied migrations are immutable history. To change the schema, add a NEW
migration rather than editing one under `migrations/*-applied/`. Hand-editing an
applied migration desynchronizes environments that already ran it.
