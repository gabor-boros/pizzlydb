# 057: Bounded variable-length paths and explicit path semantics

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-query` |
| Type | Feature |

## Summary

Support bounded multi-hop pattern segments, written `*m..n` with a finite `n`, under the three path modes `walk`, `trail` (no repeated edges), and `simple` (no repeated nodes), exactly as specified in [050](050-query-language-specification.md). An enforced maximum expansion and visited-edge budget applies, and the planner integrates cost bounds for these segments.

## Motivation

Guide §5.2 lists bounded multi-hop traversal among the supported core operations. Guide §5.3 forbids silently interchanging path semantics and requires an enforced maximum expansion. Multi-hop patterns are where graph queries blow up combinatorially, so they need dedicated operators and dedicated budget accounting.

## Current State

- Not implemented. The executor and planner come from [055](055-physical-operators-and-executor.md) and [056](056-planner-cost-model-and-explain.md).

## Scope

### In scope

- A `VarExpand` operator, depth-first or breadth-first with a frontier budget, that tracks the path state each mode needs: an edge set for `trail`, a node set for `simple`.
- Path value output, if the RFC allows returning paths, and path length.
- Planner cost bounds computed from degree quantiles raised to the power of the depth, with rejection when the bound exceeds the policy limit.
- Fixed-length expansion uses the cheaper operators, and the variable-length operator applies only where needed.

### Out of scope

- Unbounded recursion and reachability, which the guide marks as unsupported. Incremental variable-length paths are deferred to [903](903-incremental-variable-length-paths.md).

## Dependencies

- [056](056-planner-cost-model-and-explain.md): the planner.

## Acceptance Criteria

- [ ] For each path mode, the results on generated graphs with cycles, self-loops, and parallel edges equal those of the reference evaluator (differential property test).
- [ ] The worked examples in the language RFC produce the documented, mode-specific results.
- [ ] Expansion beyond the budget fails deterministically with a typed error (test).

## Testing and Validation

- Differential property tests and benchmarks for depths 2 to 5 on power-law graphs.

## References

- [Guide §5.3 path semantics](../architecture/implementation-guide.md#53-path-semantics)
- ISO GQL path modes (WALK, TRAIL, SIMPLE, ACYCLIC)
