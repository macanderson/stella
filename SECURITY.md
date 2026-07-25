# Security Policy

Stella runs shell commands, edits files, and talks to model providers on your
behalf — we take the security of that surface seriously, and we appreciate the
researchers who help keep it tight.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately via
[GitHub's private vulnerability reporting](https://github.com/macanderson/stella/security/advisories/new)
— it goes straight to the maintainers, and you get credit in the advisory when
it's published.

Include what you'd want in any good bug report: affected version/commit, a
reproduction, and your assessment of impact. We aim to acknowledge reports
within **72 hours** and to ship a fix or a mitigation plan within **30 days**
for confirmed issues, keeping you informed along the way.

## Scope — what counts

Especially interesting, given what Stella promises:

- **Workspace-root escape** — any way a tool call (file CRUD, `bash`, `grep`/`glob`)
  reaches outside the pinned workspace root: traversal, symlinks, race conditions.
- **Phone-home violations** — telemetry, update checks, or analytics leaving the
  machine in Community/default mode. Zero is the contract there, and the only
  governed exception is an explicitly enrolled Oxagen Enterprise seat. Network
  traffic the user asked for is *not* a violation: the chosen model provider,
  configured MCP servers, the opt-in `web` tools (`web_fetch` /
  `web_extract_assets` / `web_download`, and `web_search` against your own
  Brave/Tavily key), and `gh`/Linear when an issue backend is connected. Traffic
  from any of those to a host the user did not configure *is* in scope.
- **Credential exposure** — API keys leaking into logs, telemetry, error
  messages, or files with permissive modes.
- **Prompt/tool injection with impact** — untrusted content (repo files, MCP
  frames, provider responses) escalating into actions the user didn't sanction,
  beyond what the model is already trusted to do.
- **Context Graph Protocol (CGP) host boundary breaks** — providers escaping quarantine: inheriting
  credentials, ambient filesystem access, or ungated egress.
- **install.sh / release integrity** — checksum bypasses, tag/asset confusion.

Out of scope: vulnerabilities in the model providers themselves, and the
inherent risk of running an agent with `bash` access on code you don't trust —
that's the user's judgment call, not a boundary Stella claims to enforce.

## Supported versions

Pre-1.0, only the latest release (and `main`) receive security fixes.
