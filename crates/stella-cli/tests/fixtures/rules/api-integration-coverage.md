---
name: api-integration-coverage
description: API endpoint changes require integration coverage.
schema_version: 1.0-draft
record_id: dir_api_integration_coverage_v1
record_kind: directive
scope_paths:
  - src/api/**
  - docs/api/**
enforcement: advisory
confidence: 88
origin: inferred
supporting_evidence_ids:
  - ev_task_118
  - ev_task_101
  - ev_task_094
observed_at: 2026-07-20T18:30:00Z
valid_from: 2026-07-20T18:30:00Z
---

When changing an API endpoint under `src/api/`, add or update an integration
test that exercises the endpoint end to end before considering the change done.
