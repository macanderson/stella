# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security vulnerabilities.

Report privately via GitHub's [**Report a vulnerability**](https://github.com/oxageninc/stella/security/advisories/new) button on the repository's Security tab (Security → Advisories → Report a vulnerability). This opens a private channel with the maintainers.

We aim to acknowledge reports within 3 business days and to ship a fix or mitigation for confirmed high-severity issues as quickly as is practical. We'll coordinate a disclosure timeline with you and credit you in the advisory unless you prefer otherwise.

## Scope

Stella is a terminal coding agent that executes tools, runs shell commands, talks to model providers with your keys (BYOK), and connects to external MCP servers. Reports we especially care about:

- Sandbox/tool escape — an agent tool or MCP server running commands or touching files outside the intended workspace.
- Secret exposure — provider API keys leaking into logs, DuckDB telemetry, transcripts, or crash output.
- Prompt-injection paths that escalate into arbitrary command or tool execution beyond the user's intent.
- Supply-chain issues in the published binary or its dependencies.

## Supported versions

Security fixes target the latest released version and `main`. Please reproduce against the current `main` before reporting.
