# pizzlydb design documentation

This directory holds the versioned design record for pizzlydb: the target architecture, the decisions that freeze it, and the plan to production.

| Directory | Contents | Normative |
| --- | --- | --- |
| [`architecture/`](architecture/implementation-guide.md) | The implementation guide: product contract, invariants, and quality gates | Yes |
| [`adr/`](adr/README.md) | Architecture decision records ADR-001 to ADR-010 and later | Accepted ADRs only |
| [`rfcs/`](rfcs/README.md) | Requests for comments that propose behavior-changing capabilities | Accepted RFCs only |
| [`roadmap/`](roadmap/README.md) | The 112 issues that take pizzlydb to a production-grade release | No, it plans work |
| [`testing/`](testing/property-harness.md) | Property harness, seeds, and the crash simulator | No, it explains the tests |
| [`benchmarks/`](benchmarks/baseline.md) | First measured storage baseline | No, it records one machine |
| [`architecture/file-identity.md`](architecture/file-identity.md) | File id and the deferred lineage field | Yes, for the current format |

When documents disagree, the guide and accepted ADRs govern, then accepted RFCs.
