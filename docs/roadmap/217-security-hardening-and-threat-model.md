# 217: Threat model, security hardening, and supply-chain controls

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | all, plus CI |
| Type | Security |

## Summary

Write a STRIDE threat model for the deployed system. It covers:

- clients, the network, the server, the engine, storage, backups, the admin plane, and the build and release pipeline
- trust boundaries, assets, and mitigations, with traceability to the issues that implement them
- residual risks

Then close the gaps it finds:

- **Hostile input:** request limits on every protocol path and decoder hardening, with fuzzing extended from [006](006-fuzzing-infrastructure.md).
- **Information leakage:** no data in error messages.
- **Denial of service:** query-complexity limits, connection limits, and limits on slow-read attacks.
- **Supply chain:** cargo-deny, cargo-audit, SBOM generation, pinned actions, and archive provenance already exist. Add a cargo-vet review policy and reproducible-build verification, extend provenance and SBOMs to container images, and complete the disclosure policy in `SECURITY.md`.

## Motivation

Guide §9 lists the security requirements, and [070](070-resume-tokens.md) and [106](106-backup-encryption-compression-destinations.md) left integration points for this issue. A threat model is how one argues that the controls are *sufficient* and not merely present. The external review in [223](223-external-review-and-ga-gate.md) needs it as input.

## Current State

- CI runs `cargo deny`, `cargo audit`, and a CycloneDX SBOM job (`.github/workflows/ci.yml`). Actions are pinned by commit SHA. Release archives carry provenance attestations and an attested SBOM (`.github/workflows/release-please.yml`).
- Pre-commit hooks run gitleaks, Trivy, and Checkov (`README.md`, "Git hooks").
- The workspace forbids `unsafe_code`.
- `SECURITY.md` names GitHub Security Advisories as the reporting channel. It does not state supported versions, response-time targets, or a disclosure timeline.
- There is no threat model.

## Scope

### In scope

- **`docs/security/threat-model.md`.** A data-flow diagram, a STRIDE table per boundary, mitigations linked to issues, and residual risk.
- **Hardening tasks from the model.** Expected examples:
  - per-message and per-stream limits
  - query-text size limits
  - regex and complexity caps, if any regex exists
  - error-detail scrubbing
  - timing-safe comparisons for credentials
  - the HTTP/2 rapid-reset mitigation
- **Supply chain:** a dependency review policy, a `cargo vet` baseline, SBOMs and provenance for every artifact including container images, reproducible-build verification, and a `SECURITY.md` extended with supported versions, response-time targets, and a disclosure timeline.
- **Static analysis in CI:** clippy restriction lints for panics in request paths (`unwrap_used` and `expect_used` in server crates), plus `cargo geiger` reporting (unsafe code in dependencies).

### Out of scope

- The external penetration test ([223](223-external-review-and-ga-gate.md)).

## Dependencies

- [205](205-tls-transport-security.md): TLS.
- [206](206-authentication.md): authentication.
- [207](207-authorization-rbac-and-policy-epochs.md): authorization.
- [208](208-multi-tenancy-and-quotas.md): tenancy.
- [209](209-audit-logging.md): audit logging.
- [210](210-secrets-management.md): secrets.
- [006](006-fuzzing-infrastructure.md): fuzzing.

## Acceptance Criteria

- [ ] The threat model is reviewed, and every High and Critical threat has a mitigation that is implemented and tested, or explicitly accepted with a sign-off.
- [ ] Fuzz targets cover every protocol decoder and token parser, each with at least 24 cumulative CPU-hours and no open crashes.
- [ ] `SECURITY.md` states supported versions and response targets, the `cargo vet` baseline is committed, and an SBOM and provenance attestation accompany every release artifact, including container images.
- [ ] Server crates deny `unwrap_used` and `expect_used` in non-test code, and the lint is enforced in CI.

## Testing and Validation

- Fuzzing campaigns, lint enforcement, and the security suite in [218](218-security-test-suite.md).

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
- `.github/workflows/ci.yml`, `.github/workflows/release-please.yml`, `deny.toml`, `SECURITY.md`
- [OWASP Threat Modeling](https://owasp.org/www-community/Threat_Modeling)
