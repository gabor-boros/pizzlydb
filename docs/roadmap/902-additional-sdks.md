# 902: Additional SDKs (TypeScript, Python, Go, Java)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | L per language |
| Crates | none (separate repositories or `sdks/`) |
| Type | Feature |

## Summary

Add official SDKs beyond Rust, generated from the Protobuf definitions ([201](201-native-protocol-definition.md)) with hand-written ergonomic layers. Each must meet the same SDK contract: safe retries only, resume tokens preserved, and frontiers exposed. A shared cross-language conformance suite verifies them.

## Motivation

Guide §8.4 defines the contract for every SDK, and guide §13 lists additional SDKs as an expansion track. Language reach drives adoption, but each SDK carries an ongoing maintenance and correctness burden, so each language must be justified by demand.

## Current State

- Only the Rust SDK is planned ([212](212-rust-sdk.md)).

## Scope

### In scope

- **Conformance suite.** A language-neutral scenario runner covering the §16.2 scenario, retry rules, resume, and fault-injected disconnects. The Rust SDK runs it first.
- **Per-language SDKs.** Each one is prioritized by demand evidence.

### Out of scope

- Object-graph mappers.

## Dependencies

- [212](212-rust-sdk.md): the reference SDK.
- [222](222-release-engineering-and-compatibility.md): the compatibility policy.

## Acceptance Criteria

- [ ] The conformance suite exists, and the Rust SDK passes it.
- [ ] Each new SDK passes the conformance suite, including the fault-injected resume scenarios.
- [ ] Each SDK has documentation and published packages with a support policy.

## Testing and Validation

- The conformance suite in CI for each SDK.

## References

- [Guide §8.4](../architecture/implementation-guide.md#84-sdk-contract), [§13](../architecture/implementation-guide.md#13-implementation-roadmap-dependency-ordered)
