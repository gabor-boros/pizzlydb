# pizzlydb-sdk

Rust client for transactions, queries, and durable subscriptions.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-sdk` is the first client library. Its public types will expose graph identifiers, transactions, queries, cancellation, errors, stream events, and durable cursor management.

The crate depends on `pizzlydb-core` and `pizzlydb-protocol`. Integration tests for restart, dropped connections, backpressure, and resume belong with this client once the server can serve those operations.

## Modules

- `client` — a session for commits, snapshot queries, and subscription resume

## License

Licensed under the [Apache License 2.0](../../LICENSE).
