# pizzlydb-subscriptions

Snapshot bootstrap, the durable result-delta log, and resume cursors.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-subscriptions` will own the client-facing result lifecycle. A subscription starts with one committed snapshot and then receives ordered result changes after that snapshot. The crate cannot mutate committed graph state.

The intended network delivery is at-least-once, in commit-sequence order for each subscription. Acknowledgements advance a client consumption cursor. They do not change the database commit position. Expired history and an incompatible plan are explicit errors.

The crate depends on `pizzlydb-core` and `pizzlydb-incremental`.

## Modules

- `bootstrap` — the race-free snapshot phase
- `output_log` — result deltas journaled before stream delivery
- `cursor` — acknowledged cursors and resume tokens

## License

Licensed under the [Apache License 2.0](../../LICENSE).
