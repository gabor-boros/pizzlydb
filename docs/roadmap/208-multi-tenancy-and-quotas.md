# 208: Multi-tenancy and quotas

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-engine`, `pizzlydb-server`, `pizzlydb-auth` |
| Type | Feature |

## Summary

Make tenants, which are organizations that own namespaces, a first-class concept. Each tenant gets enforced quotas covering:

- storage bytes and entity counts
- query compute, measured as CPU time and visited edges per window, and query complexity caps
- concurrent transactions, queries, and subscriptions
- outbound bytes per window
- retained log and output bytes

The issue also adds per-tenant metering, metrics, and usage reports, and it tests isolation so that one tenant cannot degrade another beyond documented bounds.

## Motivation

Guide §9 states that quotas cover storage, compute, query complexity, subscription count, outbound volume, and retained logs per tenant. Invariant C8 says that no query result or durable cursor can cross a tenant or authorization boundary, and invariant C9 covers boundedness. In a shared single node, a noisy neighbor is the main operational risk.

## Current State

- After Phase 1 and Phase 2, namespaces exist ([045](045-namespace-registry-and-lifecycle.md)), resources have budgets ([117](117-resource-governance-and-memory-budgets.md)), retention is inspectable ([114](114-retention-management-and-inspection.md)), and statistics count bytes and entities ([046](046-graph-statistics.md)). No tenant model or quotas exist.

## Scope

### In scope

- **Tenant registry.** Tenant metadata in the system key space, with a tenant-to-namespaces mapping and admin RPCs for provisioning.
- **Quota model and enforcement points:**
  - storage: at commit, using a commutative counter, with an over-quota tenant rejected with `RESOURCE_EXHAUSTED`
  - compute: token buckets in the executor accounting
  - concurrency: admission control
  - outbound: delivery
  - retention: [114](114-retention-management-and-inspection.md) caps per tenant
- **Metering and reporting.** Metrics per tenant with bounded cardinality, plus usage-report export.
- **Fair scheduling.** Weighted fairness between tenants in the blocking pools and the maintenance workers.

### Out of scope

- Billing integration.

## Dependencies

- [207](207-authorization-rbac-and-policy-epochs.md): authorization scopes.
- [117](117-resource-governance-and-memory-budgets.md): budgets.
- [045](045-namespace-registry-and-lifecycle.md): namespaces.
- [114](114-retention-management-and-inspection.md): retention.

## Acceptance Criteria

- [ ] Each quota dimension is enforced with typed errors, and the metering is accurate within a documented tolerance (tests).
- [ ] Noisy-neighbor test: tenant A runs an adversarial workload while tenant B runs a steady one. B's p99 latency stays within a documented factor of its solo baseline.
- [ ] Isolation (C8): no API, including explain, error messages, metrics labels, and inspect output, reveals another tenant's data or names. This is covered with [218](218-security-test-suite.md).

## Testing and Validation

- Quota tests, noisy-neighbor benchmarks, and isolation tests.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy), [§12.2 C8/C9](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
