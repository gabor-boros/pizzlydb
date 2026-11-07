# 904: Composite, full-text, vector, and spatial indexes (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | XL |
| Crates | `pizzlydb-graph`, `pizzlydb-query`, `pizzlydb-core` |
| Type | RFC, feature |

## Summary

Extend indexing beyond the single-property B-tree indexes from Phase 1. Each extension below needs its own RFC:

- **Composite indexes and constraints** on multiple properties.
- **Full-text indexes.**
- **Vector (ANN) indexes**, with their exactness and recall semantics documented.
- **Spatial types and indexes.**

## Motivation

The guide's expansion tracks list full-text and vector indexes. [040](040-type-system-and-value-model.md), [041](041-schema-constraints-at-commit.md), and [044](044-online-index-build-and-drop.md) deliberately scope these out.

Approximate indexes interact badly with the exactness guarantee C4 (indexed and unindexed evaluation give the same result). Each RFC must therefore define how approximate results are exposed: as explicitly approximate operators, and never as a substitute for exact evaluation.

## Current State

- Phase 1 delivers single-property indexes only.

## Scope

### In scope

- **One RFC per index family,** covering the user contract, the C4 implications, and the incremental-maintenance implications. Approximate indexes are excluded from subscriptions unless the RFC proves determinism.
- **Composite indexes first**, because they are exact and low-risk.

### Out of scope

- Columnar projections, which would need a separate RFC.

## Dependencies

- [044](044-online-index-build-and-drop.md): the online index build framework.
- [056](056-planner-cost-model-and-explain.md): planner integration.

## Acceptance Criteria

- [ ] An RFC for each family is accepted.
- [ ] Composite indexes satisfy C4, verified by differential tests.
- [ ] Approximate indexes are visibly opt-in, and their recall is measured against a documented benchmark.

## Testing and Validation

- Differential and benchmark testing.

## References

- [Guide §3.4](../architecture/implementation-guide.md#34-initial-physical-index-set), [§13](../architecture/implementation-guide.md#13-implementation-roadmap-dependency-ordered)
