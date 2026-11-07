# 201: Native protocol definition (Protobuf and gRPC) with a compatibility policy

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-protocol`, new `proto/` |
| Type | Feature, compatibility |

## Summary

Define the versioned Protobuf messages and gRPC services that expose the engine API (guide §8.2):

- **Transactions:** begin, operations, commit, and rollback.
- **Graph operations:** get, create, update, delete, and detach delete.
- **Queries:** query and explain, with streaming pages.
- **Subscriptions:** subscribe (server streaming), ack, and `wait_until_maintained`.
- **Schema:** index and constraint management.
- **Admin, health, and metrics:** backup, restore, inspect retention, and maintenance controls.

Typed fields carry `CommitSeq`, `plan_revision`, frontiers, and the snapshot and delta framing. A canonical error model carries the guide's codes, typed details, and retryability. Limits, deadlines, cancellation, transaction ids, and idempotency keys are part of the contract. Compatibility checks (`buf lint` and `buf breaking`) run in CI.

## Motivation

Guide §8.1 specifies Protobuf over TLS-protected HTTP/2 gRPC, and it requires compatibility tests across adjacent protocol versions. The protocol is the public contract that SDKs and third-party clients build against, so its stability matters as much as the on-disk format's.

## Current State

- `crates/pizzlydb-protocol/src/error.rs` contains only a doc comment that lists the canonical error names.
- `lib.rs` states that "Protobuf and gRPC transport are not part of this skeleton".
- No `proto/` directory exists.

## Scope

### In scope

- **Package layout.** Files under `proto/pizzlydb/v1/*.proto`, with `buf.yaml` lint rules and breaking-change detection against the last release.
- **Value encoding.** Every type in [040](040-type-system-and-value-model.md) has an exact encoding. Ids are 16-byte `bytes`, and decimals and timestamps use explicit messages.
- **Error model.** `google.rpc.Status`, with a `PizzlyError` detail carrying the code, the reason, the retryability, and typed details (for example a conflicting key prefix or a constraint name). The engine errors from [071](071-embedded-engine-composition.md) map onto it totally.
- **Limits.** Maximum message size, maximum batch sizes, and pagination tokens.
- **Code generation.** `prost` and `tonic` code generation in `pizzlydb-protocol`, with no hand-written wire code.
- **Compatibility policy.** Field additions must not change existing semantics, enum evolution rules apply, and deprecated fields are reserved. The policy is published in `docs/specifications/protocol.md`.

### Out of scope

- Server implementation ([202](202-server-runtime.md)–[204](204-subscription-delivery-service.md)).

## Dependencies

- [071](071-embedded-engine-composition.md): the engine API and error model.
- [070](070-resume-tokens.md): the token semantics.

## Acceptance Criteria

- [ ] Every operation in guide §8.2 has a corresponding RPC (checklist in the spec document).
- [ ] `buf lint` and `buf breaking` run in CI and pass.
- [ ] Every engine error code maps to the protocol code, and the mapping is round-trip tested.
- [ ] Protocol decoders are fuzzed through the generated types, with message-size limits enforced before allocation.

## Testing and Validation

- Mapping tests, `buf` checks, and fuzzing.

## References

- [Guide §8.1–§8.3](../architecture/implementation-guide.md#8-api-protocol-and-sdks)
- `crates/pizzlydb-protocol/src/{lib,error}.rs`
- [Buf breaking-change detection](https://buf.build/docs/breaking/)
