# 221: Documentation set: user, operator, reference, and contributor docs

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (critical path) |
| Size | L |
| Crates | none (`docs/`) |
| Type | Documentation |

## Summary

Produce the complete documentation set:

- **Getting started:** install, first graph, first query, and first subscription.
- **Concepts:** the data model, transactions and isolation, `CommitSeq` and frontiers, subscriptions and at-least-once delivery, and resume tokens.
- **Query language reference** ([050](050-query-language-specification.md)).
- **SDK guide** ([212](212-rust-sdk.md)).
- **Operations guide:** deployment, configuration reference, backup and restore, monitoring and alerts, capacity planning, upgrades, and the runbooks from [216](216-packaging-and-deployment.md).
- **Security guide:** TLS, authentication, RBAC, tenancy, audit, and secrets.
- **Limits and guarantees page:** every bound, every supported operation, and explicit non-goals.
- **Error-code reference**, with retry guidance.
- **Contributor and architecture docs**, consolidated from [001](001-design-record-and-baseline-adrs.md).

The docs site is built and link-checked in CI. Every code sample is a compiled test.

## Motivation

Guide §15 includes documentation in the definition of done. Operators cannot run safely what is not documented, and the at-least-once and frontier semantics are subtle enough that wrong usage is likely without precise docs.

## Current State

- The root `README.md` covers the crate layout, development commands, hooks, CI, and releases. Each crate has a `README.md` describing its contract. `SECURITY.md` names the reporting channel.
- `docs/` holds only design documents: the implementation guide, ADRs, RFCs, and this roadmap ([001](001-design-record-and-baseline-adrs.md)).
- There are no user guides, operator guides, or reference pages.

## Scope

### In scope

- **Structure and tooling:** mdBook or similar, CI build, a link checker, and doctested samples (`skeptic`-style or mdBook test).
- **Generated references.** The configuration reference comes from the schema in [101](101-configuration-system.md), the metrics reference from the registry in [102](102-metrics-foundation.md), and the error codes from the protocol in [201](201-native-protocol-definition.md).
- **Versioned docs** per release.
- Updates to `README.md` and `CHANGELOG` practices, following the repository rule.

### Out of scope

- Marketing site.

## Dependencies

- [050](050-query-language-specification.md): the query language.
- [211](211-admin-api.md): the admin API.
- [213](213-cli.md): the CLI.
- [216](216-packaging-and-deployment.md): deployment.

## Acceptance Criteria

- [ ] Every public API, configuration key, metric, error code, and CLI command appears in the generated references. CI fails if any item is missing.
- [ ] All code samples compile and run in CI.
- [ ] A new-user walkthrough is validated by someone outside the core team, and their findings are addressed.
- [ ] The limits and guarantees page is reviewed against the implemented bounds. Each bound links to the test that enforces it.

## Testing and Validation

- Docs CI with link checks and sample tests.

## References

- [Guide §15](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
- `README.md`, `SECURITY.md`, `crates/*/README.md`
