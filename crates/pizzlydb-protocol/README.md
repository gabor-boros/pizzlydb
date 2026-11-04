# pizzlydb-protocol

Versioned native protocol types and canonical client error names.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API. Protobuf messages and gRPC transport are not in this crate yet.

## Purpose

`pizzlydb-protocol` will define the versioned wire types for transactions, queries, and subscriptions. The intended transport is Protobuf over TLS-protected HTTP/2 gRPC. Commit sequence, plan revision, and snapshot or delta framing are typed fields.

The crate depends on `pizzlydb-core`.

## Error names

The intended canonical names are:

- `INVALID_ARGUMENT`
- `QUERY_UNSUPPORTED`
- `QUERY_TOO_EXPENSIVE`
- `TX_CONFLICT`
- `TX_EXPIRED`
- `CONSTRAINT_VIOLATION`
- `NOT_FOUND`
- `UNAUTHENTICATED`
- `PERMISSION_DENIED`
- `RESOURCE_EXHAUSTED`
- `SLOW_CONSUMER`
- `CURSOR_EXPIRED`
- `PLAN_INVALIDATED`
- `RESUME_FORBIDDEN`
- `UNAVAILABLE`
- `INTERNAL`

Retry behavior is documented per error. The SDK does not automatically retry a mutation that may not be idempotent unless the caller supplies an idempotency key.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
