# Architecture decision records

An architecture decision record (ADR) freezes one architectural choice. Guide [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process) requires ADR-001 through ADR-010. Changing an accepted decision requires a new ADR that supersedes it.

## Status values

- **Reserved:** the number and owner issue are assigned, and no decision is recorded yet.
- **Proposed:** the decision is written and under review.
- **Accepted:** the decision is in force, and the code is expected to follow it.
- **Superseded by ADR-NNN:** a later ADR replaced the decision. The file stays for history.

## Index

| ADR | Decision | Status | Owner issues |
| --- | --- | --- | --- |
| [ADR-001](0001-rust-workspace-and-unsafe-policy.md) | Rust workspace, edition 2024, forbidden unsafe code, crate dependency direction | Accepted | [001](../roadmap/001-design-record-and-baseline-adrs.md) |
| [ADR-002](0002-identity-and-value-model.md) | Identity, value types, key encoding, schema model | Reserved | [034](../roadmap/034-id-allocation-and-identity-policy.md), [040](../roadmap/040-type-system-and-value-model.md) |
| [ADR-003](0003-page-engine-and-ordered-keys.md) | In-house page engine with graph data as ordered keys | Accepted | [001](../roadmap/001-design-record-and-baseline-adrs.md) |
| [ADR-004](0004-transaction-isolation.md) | Transaction isolation, concurrency control, conflicts | Reserved | [030](../roadmap/030-adr-transaction-isolation-model.md) |
| [ADR-005](0005-commit-sequence-and-mutation-log.md) | Logical mutation log and `CommitSeq` invariants | Reserved | [031](../roadmap/031-commit-sequence-and-commit-metadata.md), [032](../roadmap/032-logical-mutation-log.md) |
| [ADR-006](0006-query-language.md) | Query grammar, result multisets, path semantics | Reserved | [050](../roadmap/050-query-language-specification.md) |
| [ADR-007](0007-incremental-execution-engine.md) | Incremental operators, retractions, plan lifecycle | Reserved | [060](../roadmap/060-adr-incremental-execution-engine.md) |
| [ADR-008](0008-durable-subscriptions.md) | Subscription protocol, acknowledgement, replay, expiry | Reserved | [068](../roadmap/068-durable-subscriptions-core.md) |
| [ADR-009](0009-authorization-and-tenant-isolation.md) | Authorization, tenant isolation, token binding | Reserved | [207](../roadmap/207-authorization-rbac-and-policy-epochs.md) |
| [ADR-010](0010-retention-and-recovery-compatibility.md) | Retention, backup and restore, checkpoint compatibility | Reserved | [114](../roadmap/114-retention-management-and-inspection.md) |
| [ADR-011](0011-storage-concurrency.md) | Storage lock order, `Send + Sync`, durability modes | Accepted | [016](../roadmap/016-thread-safe-storage-engine.md) |
| [ADR-012](0012-storage-format-policy.md) | Superblock minor, feature flags, and WAL generation | Accepted | [024](../roadmap/024-storage-format-versioning-and-golden-fixtures.md) |

Other roadmap issues also produce ADRs: an amendment to ADR-001 for a new composition crate ([071](../roadmap/071-embedded-engine-composition.md)). New ADRs take the next free number, starting at ADR-013.

## Writing an ADR

1. Copy [`template.md`](template.md) to `NNNN-short-title.md`, or replace the reserved file for that number.
2. Fill in every section. Cite code by file and symbol, and link the guide sections that the decision implements.
3. Update the index above and any guide section that the decision changes, in the same change.
4. Set the status to Accepted when the owner issue's acceptance criteria are met.
