# 907: Sharding and distributed execution (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Won't (v1) |
| Size | XXL |
| Crates | new |
| Type | RFC |

## Summary

Research and write an RFC for horizontal partitioning: graph partitioning, cross-shard traversals, cross-shard transactions and commit ordering, distributed incremental maintenance, and a global or causal `CommitSeq`.

## Motivation

Guide §10.5 says that sharding and cross-shard transactions are not implied by the initial API and must not be introduced without explicit evidence and compatibility review. This issue exists so that the decision is recorded and not forgotten. It is classified **Won't (v1)**.

## Current State

- Out of scope by design. Phase 1 makes no guarantee about how APIs map to shards.

## Scope

### In scope

- Collect workload evidence that single-node vertical scaling plus [901](901-replication-and-high-availability.md) read replicas are insufficient.
- Write the RFC with alternatives, including application-level partitioning by namespace or tenant.

### Out of scope

- Implementation until the RFC is accepted.

## Dependencies

- [901](901-replication-and-high-availability.md): replication.

## Acceptance Criteria

- [ ] An evidence report and a decision record exist, whether or not the RFC proceeds.

## Testing and Validation

- None until the RFC is accepted.

## References

- [Guide §10.5](../architecture/implementation-guide.md#105-high-availability-and-distribution)
