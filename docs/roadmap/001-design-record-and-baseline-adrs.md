# 001: Establish the design record and the baseline ADRs

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must |
| Size | M |
| Crates | none (documentation) |
| Type | Documentation, governance |

## Summary

Keep the implementation guide, the ADRs, the RFCs, and this roadmap in a versioned `docs/` tree whose links CI checks. Record the two decisions the code already embodies as ADR-001 and ADR-003, and reserve ADR-002 and ADR-004 through ADR-010 for the issues that own them.

## Motivation

Guide §14 requires ADR-001 through ADR-010 before the work each one governs. Later issues write those ADRs and new RFCs, so they need a reviewed home, a defined process, and links that cannot break silently.

Two decisions are implemented but not recorded: the Rust workspace with its unsafe-code policy, and the in-house page engine that stores all graph state as ordered keys. Without a record, a later change can reverse either one by accident.

## Current State

- No ADR files exist, although guide §14 lists ADR-001 through ADR-010 as mandatory.
- The guide refers to `docs/specifications/`, which does not exist.
- No CI job checks documentation links or heading anchors.

## Scope

### In scope

- Create `docs/` with `architecture/` (the guide), `adr/`, `rfcs/`, and `roadmap/`, plus an index page.
- Write ADR-001 (Rust workspace, edition 2024, `unsafe_code = "forbid"`, crate dependency direction) and ADR-003 (in-house page engine with an ordered-key B+ tree instead of an embedded KV engine). Both decisions are already implemented.
- Create placeholder ADR files that name the issue owning each remaining decision:
  - ADR-002 (identity and value types): [034](034-id-allocation-and-identity-policy.md) and [040](040-type-system-and-value-model.md)
  - ADR-004 (isolation): [030](030-adr-transaction-isolation-model.md)
  - ADR-005 (commit log): [031](031-commit-sequence-and-commit-metadata.md) and [032](032-logical-mutation-log.md)
  - ADR-006 (query language): [050](050-query-language-specification.md)
  - ADR-007 (incremental engine): [060](060-adr-incremental-execution-engine.md)
  - ADR-008 (subscriptions): [068](068-durable-subscriptions-core.md)
  - ADR-009 (authorization): [207](207-authorization-rbac-and-policy-epochs.md)
  - ADR-010 (retention): [114](114-retention-management-and-inspection.md)
- Write ADR and RFC index pages that define status values and numbering, and a template for each with the sections guide §14 requires.
- Resolve or remove the guide's references to `docs/specifications/`.
- Add a CI job that runs an offline link checker (`lychee`) over `docs/**/*.md`.

### Out of scope

- Writing the remaining decision ADRs and the design RFCs; those belong to the issues that own each decision.

## Dependencies

- None. This is the first issue and unblocks every ADR-writing issue.

## Acceptance Criteria

- [ ] `docs/` is tracked in git, and its index page links every subdirectory.
- [ ] ADR-001 and ADR-003 are accepted, and the ADR index lists ADR-002 and ADR-004 through ADR-010 with their owning issues.
- [ ] The ADR and RFC indexes define their status values and numbering, and each has a template.
- [ ] The guide's references to `docs/specifications/` either resolve or are removed.
- [ ] The CI docs job runs the link checker, and passes.

## Testing and Validation

- Run the link checker on all Markdown under `docs/`.
- Manual review: run the verification commands in ADR-001 and ADR-003 and confirm that they pass.

## References

- [Implementation guide §14 (mandatory ADR/RFC process)](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
- [ADR index](../adr/README.md), [RFC index](../rfcs/README.md)
