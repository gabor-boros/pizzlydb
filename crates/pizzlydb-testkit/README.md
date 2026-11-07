# pizzlydb-testkit

Reference graph, seeded histories, benchmark datasets, and the JSON report used to compare two runs.

The crate depends on `pizzlydb-core` only. It does not open a database. Engine adapters live in the crate under test. The graph adapter is `crates/pizzlydb-graph/tests/model.rs`.

## Public API

- `ReferenceGraph`, `Operation`, `History`, `StepError`, `GraphSnapshot`
- `generate_history`, `history_strategy`, `WorkloadBias`, `PROPERTY_STEPS`
- `Subject`, `run_history`, `minimize`, `Divergence`
- `uniform_graph`, `power_law_graph`, `social_graph`
- `BenchReport`, `BenchResult`, `compare_reports`, `report_to_json`, `report_from_json`

`compare-bench` reads two JSON reports and exits 1 when a median moves by more than the tolerance (default 0.05):

```shell
cargo run -p pizzlydb-testkit --bin compare-bench -- left.json right.json 0.05
```

The [property harness](../../docs/testing/property-harness.md) explains seeds and the crash simulator.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
