# pizzlydb-server

Runtime composition of storage, transactions, query, maintenance, and delivery.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API. It is a library and has no process entrypoint.

## Purpose

`pizzlydb-server` is the composition root. It will wire authentication, protocol, transactions, graph access, query, incremental maintenance, subscriptions, and storage. Startup will verify storage metadata, recover the durable commit frontier, and publish subscriptions at that frontier.

The crate depends on `pizzlydb-auth`, `pizzlydb-protocol`, `pizzlydb-txn`, `pizzlydb-graph`, `pizzlydb-query`, `pizzlydb-incremental`, `pizzlydb-subscriptions`, and `pizzlydb-storage`.

## Modules

- `runtime` — the server handle and recovered frontier

The admin and query shell is the `pizzlydb` binary in [`pizzlydb-cli`](../pizzlydb-cli/README.md).

## License

Licensed under the [Apache License 2.0](../../LICENSE).
