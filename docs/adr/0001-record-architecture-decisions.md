# 1. Record architecture decisions

- Status: accepted
- Date: 2026-11-07

## Context

Spur is a long-lived project with a minimal core and many extensions. Decisions about the core, the protocol and the extension API are expensive to reverse and need to be discoverable later, by humans and by agents working on the codebase.

## Decision

We record significant architecture decisions as Architecture Decision Records in `docs/adr/`, following Michael Nygard's format.

- One file per decision: `NNNN-short-title.md`, numbered sequentially.
- Sections: Status, Context, Decision, Consequences.
- Status is one of: proposed, accepted, superseded by NNNN, deprecated.
- ADRs are immutable once accepted; change course with a new ADR that supersedes the old one.
- A decision made in a GitHub issue is linked from its ADR.

## Consequences

- Rationale stays next to the code and is reviewed in PRs.
- Contributors and agents can read `docs/adr/` to learn why things are the way they are.
- Small overhead per decision; reserved for choices that affect architecture, protocols, public APIs or dependencies.
