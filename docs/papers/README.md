# Research papers

Original analysis on Stella's architecture, the Context Graph Protocol (CGP), and
the defensible properties of deterministic coding-agent design. These are
research-grade documents — written for engineers and architecture reviewers,
grounded in primary research, and referencing the shipping implementation by
file and line.

## Papers

- [**Self-Evolving Coding Agents — comparison with Stella**](./self-evolving-coding-agents-assessment.md)
  — compares Stella 0.8.40's shipped and planned self-improvement surfaces
  with Zhou et al.'s 2026 survey and extracts concrete backlog considerations.

- [**Stella: A Defensible Technology Position**](./stella-defensible-position.md)
  — the capstone analysis. Identifies seven architectural invariants that make
  Stella's design expensive to replicate and shows why their *combination* —
  not any single property — constitutes the moat. Covers ports-not-concretions,
  no-I/O-in-the-engine, the witness-test contract, BYOK + no-phone-home,
  prompt-cache-native memory, budget enforcement at safe boundaries, and the
  Context Graph Protocol.

- [**The Deterministic Engine: Why Single-Thread Beats the Swarm**](./deterministic-engine.md)
  — a focused analysis of one defensible property: Stella's decision to build
  a single-thread deterministic engine rather than a multi-agent swarm. Draws
  on the MAST (Multi-Agent System Failure Taxonomy) findings from UC Berkeley
  and the success of Agentless and SWE-agent to argue that determinism is a
  feature, not a limitation.

## Related

- [**The Context Graph Protocol: Advantages and Uniqueness**](https://github.com/macanderson/context-graph-protocol/blob/main/docs/protocol-advantages.md)
  — standalone analysis of the CGP's trust architecture: the seven advantages
  (provenance, budget honesty, consent enforcement, conformance verification,
  citation guarantees, version stability, temporal validity) and why the
  combination is irreducible.

---

*Every claim in these papers is grounded in the shipping implementation. When
a paper references a property, it links to the code that enforces it.*
