# pizzlydb-query

Graph Query Language (GQL)-aligned parser, typed intermediate representation (IR), planner, and snapshot executor.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-query` will parse a documented GQL-aligned subset, including the pizzlydb `SUBSCRIBE` extension. It does not claim complete ISO GQL conformance.

The pipeline is parse, bind names and types, authorize, normalize to typed IR, estimate cost, choose access paths, and execute a snapshot plan. The IR is shared with `pizzlydb-incremental`. Unsupported queries fail at planning time with a precise reason.

The crate depends on `pizzlydb-core` and `pizzlydb-graph`.

## Modules

- `parser` — the query subset and `SUBSCRIBE`
- `ir` — the language-independent typed IR
- `planner` — access paths, cost checks, and plan explanation
- `executor` — snapshot execution against one graph read view

## License

Licensed under the [Apache License 2.0](../../LICENSE).
