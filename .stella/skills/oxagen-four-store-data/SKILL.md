---
name: oxagen-four-store-data
description: The four-store data boundary in Oxagen — Postgres/Drizzle conventions (UUID id + public_id, soft delete), Neo4j via ontology contracts, ClickHouse append-only event emission, and blob storage. Use when designing a schema, adding a table, writing a graph mutation, or emitting telemetry — and to check which store a piece of data belongs in.
---

# Oxagen four-store data model

Four stores, strict boundaries. **Never:** analytics in Neo4j, graph relationships in Postgres, transactional state in ClickHouse, binary payloads in any DB.

- **PostgreSQL** — transactional state only: users, orgs, permissions, billing, configs, job metadata, durable application state.
- **Neo4j** — graph data only: ontology/entity relationships, workflow lineage, agent memory, semantic retrieval.
- **ClickHouse** — append-only runtime events only: execution events, logs, metrics, traces, token analytics, tool usage, telemetry.
- **File/blob storage** — binary assets only (avatars, generated images/video/docs, uploads); the Postgres row (URL + metadata) is the source of truth. Driver: Vercel Blob via `@oxagen/storage`.
- **Exception — Connector Dual-Write:** data connectors write to Postgres (operational record: sync cursor, connection health — source of truth, ACID) *and* Neo4j (graph index: entities, embeddings, relationships — async Inngest, retryable). ClickHouse observes ingestion events for telemetry only.

## Postgres / Drizzle

One schema file per domain: `packages/database/src/schema/<domain>.ts` (`org.ts`, `billing.ts`, `agent.ts`, `iam.ts`, ...). TS var camelCase, SQL identifier snake_case via `.table("snake_case_name", {...})`. Real example, `packages/database/src/schema/org.ts`:

```ts
export const organizations = orgSchema.table(
  "organizations",
  {
    ...idMixin("org"),
    ...auditMixin(),
    name: text("name").notNull(),
    slug: citext("slug").notNull(),
    // ...
  },
  (t) => ({
    slugIdx: uniqueIndex("organizations_slug_idx").on(t.slug),
    statusCheck: check("organizations_status_check", sql`${t.status} IN ('active','suspended','deleted')`),
  }),
);
```

### `idMixin` / `auditMixin` / `softDeleteMixin` (`packages/database/src/schema/_mixins.ts`)

```ts
export const idMixin = (publicIdPrefix: string) => ({
  id: uuid("id").primaryKey().default(uuidv7Default),
  publicId: citext("public_id").notNull().unique()
    .$defaultFn(() => `${publicIdPrefix}_${cryptoRandom(22)}`),
});

export const auditMixin = () => ({
  createdAt: timestamp("created_at", { withTimezone: true, mode: "date" }).notNull().defaultNow(),
  updatedAt: timestamp("updated_at", { withTimezone: true, mode: "date" }).notNull().defaultNow(),
  createdByUserId: uuid("created_by_user_id"),
  updatedByUserId: uuid("updated_by_user_id"),
});

/** soft_delete_mixin — hard deletes prohibited on org-scoped tables. */
export const softDeleteMixin = () => ({
  deletedAt: timestamp("deleted_at", { withTimezone: true, mode: "date" }),
  deletedByUserId: uuid("deleted_by_user_id"),
});
```

- Internal id is a UUIDv7 (`id`), never exposed externally.
- `public_id` is a citext, prefixed (`org_`, `wrk_`, `agt_`, ...), unique, and is what the API/UI ever shows or accepts from clients — never leak the raw `id` as a user-facing identifier (see `oxagen-naming` for the UI corollary on node/edge citations).
- Org-scoped tables get `orgScopeMixin()` (`orgId`, `workspaceId`, both required).
- Hard deletes are prohibited on org-scoped tables — use `softDeleteMixin()` (`deletedAt`/`deletedByUserId`), never `DELETE FROM`.

Migrations live under `packages/database/atlas/migrations/`, **never** under `apps/`. After adding/renaming a migration, regenerate the checksum: `atlas migrate hash --dir "file://atlas/migrations"` from `packages/database` (never hand-edit `atlas.sum`).

## Neo4j / `@oxagen/ontology`

`packages/ontology/src/client.ts` — a singleton driver per process (`driver()`); `session()` binds the configured database. Mutations live under `packages/ontology/src/mutations/<verb-noun>.ts` — one file per graph write operation, kebab-case, matching the capability it backs (`acquire-file-lock.ts`, `record-execution.ts`, `release-file-lock.ts`).

The `ontology.*` graph query layer is wired: `ontology.neighbors` and `ontology.query` have contracts, API routes, and MCP tools. **Call them via `invoke()`/the contract — never touch the Neo4j driver directly from application code outside `packages/ontology`.**

## ClickHouse / `@oxagen/telemetry`

`packages/telemetry/src/` splits by concern:
- `usage-events.ts` — public/CLI-safe schema, zod `.strict()` allowlist, exported as its own subpath `@oxagen/telemetry/usage-events` so the CLI doesn't pull in the full analytics backend.
- `security.ts` — audit events.
- `skill-telemetry.ts`, `execution-diagnostics.ts`, `error-clusters.ts`, `error-reporting.ts` (`captureError`, used by the API's global error handler — see `oxagen-error-handling`).

Event schemas favor short lowercase identifier tokens and explicit `BoolFlag` (0/1 union), not JS truthy/falsy, to keep ClickHouse columns tightly typed. All writes are append-only — never `UPDATE`/`DELETE` a ClickHouse row.

## Blob storage

`@oxagen/storage` (Vercel Blob, `BLOB_READ_WRITE_TOKEN`) — binary bytes only. The Postgres row (URL + metadata) is the source of truth. Real pattern from `documentsGenerateHandler` (`packages/handlers/src/document.generate.ts`): `persistGeneratedAsset()` uploads bytes and returns `{ id, publicId, url, serveUrl, sizeBytes }` for the Postgres asset row.

## Cross-store rule

`packages/oxagen` (kernel/tenancy) never imports a ClickHouse/Neo4j client directly — it accepts **injected callbacks** (`setSecurityEventEmitter`, `setKernelTraceSink`, `setBillingAdmissionGate`) at surface bootstrap, so the kernel stays free of heavy dependency chains and each store's client lives only in its own package.

## Violations to avoid

- Storing a graph relationship as a Postgres foreign-key join table instead of a Neo4j edge.
- Writing execution/telemetry events into Postgres instead of ClickHouse (or vice versa — never mutate/query ClickHouse for transactional truth).
- Storing binary bytes (base64 blobs, file contents) in a Postgres column or a ClickHouse column instead of blob storage + a URL reference.
- Exposing the raw internal `id` (UUID) to a client/API response instead of `public_id`.
- Hard-deleting a row on an org-scoped table instead of setting `deletedAt`/`deletedByUserId`.
- Importing the Neo4j driver directly from application/handler code instead of going through `ontology.*` contracts or `packages/ontology/src/mutations/*`.
- Hand-editing `atlas.sum` instead of regenerating it with `atlas migrate hash`.
