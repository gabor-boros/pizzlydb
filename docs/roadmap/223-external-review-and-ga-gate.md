# 223: External review and general-availability gate

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities (final gate, M7) |
| Priority | Must (milestone gate M7, critical path) |
| Size | M |
| Crates | none |
| Type | Gate |

## Summary

General availability (GA) is the final gate before calling pizzlydb production-grade. The gate needs:

- An **independent security assessment**: a penetration test of the deployed system and a review of the authorization, token, and tenancy design.
- An **independent correctness review**: of the transaction and incremental-maintenance designs and the evidence for invariants C1–C9, S1–S3, R1–R2, and A1. This can be an academic or industry reviewer, or a Jepsen-style engagement.
- **Verification that every item in the definition of done in guide §15 is met**, with linked evidence.
- **Closure of every finding**, either fixed or accepted with a documented rationale.

## Motivation

Guide §13 M8 ("Public production-readiness candidate") and §15 define the end state. Self-assessment cannot catch systematic blind spots. For a database that makes isolation and delivery guarantees, an external review is a standard industry expectation before a production claim.

## Current State

- The prerequisites are the rest of this roadmap.

## Scope

### In scope

- **Review packages:** the threat model ([217](217-security-hardening-and-threat-model.md)), the isolation ADR and verification evidence ([030](030-adr-transaction-isolation-model.md), [036](036-isolation-verification-suite.md)), the incremental ADR and evidence ([060](060-adr-incremental-execution-engine.md), [067](067-incremental-correctness-suite.md)), and the chaos results ([220](220-chaos-soak-and-simulation-testing.md)).
- **Engagements:** security and correctness reviewers, a defined scope, and a final report.
- **Remediation tracking**, then re-test.
- **Definition-of-done audit:** every item in guide §15 and the production-readiness checklist in the [roadmap README](README.md#9-production-readiness-checklist) is checked with an evidence link.
- **GA announcement prerequisites:** docs version, a compatibility statement, and a support policy.

### Out of scope

- Certifications such as SOC 2, which depend on the deployment rather than the product.

## Dependencies

- [072](072-phase-1-exit-soak-and-invariant-audit.md): the Phase 1 audit.
- [119](119-operational-fault-injection-suite.md): the Phase 2 gate.
- [217](217-security-hardening-and-threat-model.md): the threat model.
- [218](218-security-test-suite.md): the security suite.
- [219](219-benchmark-suite-and-slos.md): SLOs.
- [220](220-chaos-soak-and-simulation-testing.md): chaos and soak.
- [221](221-documentation-set.md): docs.
- [222](222-release-engineering-and-compatibility.md): release engineering.

## Acceptance Criteria

- [ ] The external security report has no open Critical or High findings.
- [ ] The external correctness review has no open findings that violate any invariant.
- [ ] Every item in guide §15 and in the production-readiness checklist links to evidence.
- [ ] Every Must issue in this roadmap is closed.

## Testing and Validation

- The re-test results for every remediation.

## References

- [Guide §13 M8](../architecture/implementation-guide.md#m8--public-production-readiness-candidate), [§15](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
