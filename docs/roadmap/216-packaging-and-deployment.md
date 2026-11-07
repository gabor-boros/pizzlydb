# 216: Packaging and deployment: container image, systemd unit, Kubernetes manifests, and runbooks

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | none (new `deploy/`) |
| Type | Deployment |

## Summary

Ship supported deployment artifacts:

- **Container image.** Minimal and distroless. It runs as a non-root user with a read-only root filesystem except for the data volume. It is signed, carries an SBOM and provenance, and is scanned for vulnerabilities.
- **systemd unit**, with hardening directives.
- **Kubernetes manifests or a Helm chart.** A StatefulSet with a single replica and a persistent volume, probes wired to [202](202-server-runtime.md), a `PodDisruptionBudget`, resource requests and limits that match the memory budgets ([117](117-resource-governance-and-memory-budgets.md)), and a security context.
- **Supported-platform matrix:** Linux x86-64 and aarch64, ext4 and xfs, and the kernel minimum.
- **Operator runbooks** for install, upgrade, backup, restore, disk full, corruption, and capacity.

## Motivation

Guide §10.1 targets a single-node deployment as a container or service with health probes. The release workflow already builds signed archives; this issue covers the rest of what operators need to run the database safely.

## Current State

- `.github/workflows/release-please.yml` builds `pizzlydb-cli` archives for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` with provenance attestation.
- There is no container image, no service unit, and no Kubernetes material.

## Scope

### In scope

- **`deploy/container/Containerfile`.** A multi-stage build with a pinned base digest, a non-root UID, and no shell in the final image. It is built and pushed in the release workflow with cosign signing, a provenance attestation, and a CycloneDX SBOM that matches the existing archive SBOM.
- **`deploy/systemd/pizzlydb.service`.** Includes `ProtectSystem=strict`, `NoNewPrivileges`, `LimitNOFILE`, and `ReadWritePaths` limited to the data directory.
- **`deploy/kubernetes/`** manifests or chart, with `fsGroup`, `terminationGracePeriodSeconds` aligned with the drain deadline, and the probes.
- **Runbooks** in `docs/operations/`.
- **CI jobs:** a container vulnerability scan (for example Trivy), a smoke test that starts the image and runs the §16.2 scenario, and the full test suite (including the crash suite from [025](025-storage-model-based-and-crash-suite.md)) on an aarch64 Linux runner. Today aarch64 is only cross-built, never tested.

### Out of scope

- A Kubernetes operator or HA topologies ([901](901-replication-and-high-availability.md)).

## Dependencies

- [202](202-server-runtime.md): the server runtime.
- [101](101-configuration-system.md): configuration.
- [104](104-engine-lifecycle-and-health.md): lifecycle and health.

## Acceptance Criteria

- [ ] The image runs as non-root with a read-only root filesystem and passes the smoke test in CI.
- [ ] Every platform in the supported-platform matrix runs the full test suite in CI. A platform that is only built is not listed as supported.
- [ ] The image is signed and has an SBOM, and the vulnerability scan passes the documented policy.
- [ ] A Kubernetes deployment on kind survives a pod deletion with no acknowledged commit lost and resumes subscriptions (CI job).
- [ ] Every runbook has been executed at least once in a drill, and the drill is recorded.

## Testing and Validation

- Image smoke test, kind-based tests, and drills.

## References

- [Guide §10.1](../architecture/implementation-guide.md#101-deployment)
- `.github/workflows/release-please.yml`
