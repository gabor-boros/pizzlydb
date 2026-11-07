# 064: Maintained-state persistence, the output-delta log, and atomic progress

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-incremental` (`checkpoint`, `arrangement`), `pizzlydb-txn`, `pizzlydb-storage` |
| Type | Feature, on-disk format |

## Summary

Persist each maintained plan in the same storage engine as the graph, under the system key space and scoped by namespace. The persisted state consists of:

- the plan definition
- the arrangements and aggregate state
- the per-plan output-delta log, keyed by `(plan, revision, commit_seq, batch_index)`
- the plan's `processed_seq`

For every processed commit batch, the state changes, the output deltas, and the new `processed_seq` are written in one storage commit. That commit is a system commit: it does not consume a `CommitSeq`. Recovery restores the state and replays the log from `processed_seq + 1` without applying anything twice.

## Motivation

Guide §6.4 requires checkpointed state and output deltas to advance in one recoverable atomic step. Guide §6.3 requires that only output deltas proven durable at or below the frontier are published, and that replay never applies deltas twice. Keeping everything in the storage engine has two benefits:

- A single atomic commit covers all of it, so there is no two-phase protocol.
- A single physical snapshot captures graph, log, and maintained state consistently for backup ([105](105-online-consistent-backup.md)).

## Current State

- `crates/pizzlydb-incremental/src/checkpoint.rs` is a one-line stub.

## Scope

### In scope

- **Key layout.** Namespaced plan keys: `sys/plan/<ns>/<plan_id>/...` for the definition, `.../state/...` for arrangements and groups, `.../out/<revision>/<seq>/<batch>` for output deltas, and `.../progress` for `processed_seq`. Plan state is versioned.
- **Storage-backed arrangements** that implement the [063](063-incremental-operators.md) trait.
- **Output chunking.** Large outputs are chunked inside one logical commit boundary (guide §7.5), and each chunk carries `(commit_seq, batch_index, last_in_commit)`.
- **Atomic progress.** One storage commit per processed batch carries the state, the output, and `processed_seq`. Batches may span several graph commits, but each output delta stays attributed to its commit.
- **Recovery.** Load the progress record, replay the log from `processed_seq + 1`, and prove there is no double-apply: state at `processed_seq` combined with replay gives the same result as continuous processing.
- **State bounds.** State-size accounting against the plan's limits.

### Out of scope

- Plan installation and invalidation ([065](065-plan-installation-and-lifecycle.md)), and output-log retention for consumers ([068](068-durable-subscriptions-core.md), [114](114-retention-management-and-inspection.md)).

## Dependencies

- [063](063-incremental-operators.md): the operators.
- [032](032-logical-mutation-log.md): the log source.

## Acceptance Criteria

- [ ] Crash simulation at every VFS operation during maintenance shows that after recovery, the state and the output log equal those of an uninterrupted run (the no-double-apply rule of guide §6.3, and the engine-side half of invariant C7).
- [ ] No output delta is readable for a commit greater than `processed_seq` (test).
- [ ] A single commit that produces 1 million output rows is chunked, and the output-log reader reassembles the commit boundary (test).
- [ ] Recovery time is proportional to the log tail after `processed_seq`, not to the graph size (benchmark).

## Testing and Validation

- Crash simulation, property tests, and benchmarks.

## References

- [Guide §6.3, §6.4](../architecture/implementation-guide.md#63-update-processing), [§7.4](../architecture/implementation-guide.md#74-persisted-output-and-cursors), [§12.2 C7](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
