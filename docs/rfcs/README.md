# Requests for comments

A request for comments (RFC) proposes a behavior-changing capability before it is treated as a project decision. Guide [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process) defines when an RFC is required. An [ADR](../adr/README.md) freezes the resulting choice.

No RFCs were open when this directory was created. Accepted requests start at [0001](0001-type-system.md).

## Status values

- **Draft:** under discussion and not normative.
- **Accepted:** reviewed. Its open questions are resolved, and its invariants, failure model, and validation plan are feasible. Implementation may still be pending.
- **Rejected:** considered and deliberately not adopted.
- **Superseded by** a linked RFC or ADR: replaced. A material change after acceptance requires a superseding document.

## Writing an RFC

1. Copy [`template.md`](template.md) to `NNNN-short-title.md`, using the next free four-digit number, starting at 0001.
2. Fill in every section. The template contains the sections that guide §14 requires.
3. Open a pull request with the status set to Draft, and link the roadmap issue that owns the decision.
