# Default Library candidate ownership measurement (DB-07)

## Scope and method

Design-only characterization; no ledger data was changed and no derived ownership index was implemented. The `default_candidate_growth_and_ownership_parity` test compares current SQL candidates after the existing owner filter with the owned row set across unmatched paths, nested roots, equal-depth ambiguity, a non-canonical stored path, a second media kind, and a frozen library. It also records that the global ledger read is cold and asserts the documented absence of `browse_ledger_owners`; stale-index rejection is consequently not exercisable until that interface exists. This is not a product regression test and does not claim DB-07 is fixed.

The ignored `default_candidate_scan_size_and_write_cost` test inserts unrelated ledger rows cumulatively and reports current candidate scan wall time, cumulative insert time, and `library.db` bytes. Measured on this run:

| Newly inserted batch | Cumulative unrelated rows | Candidate rows returned | Candidate read (ms) | Batch insert time (ms) | `library.db` bytes |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 1,000 | 1,000 | 2 | 187 | 712,704 |
| 10,000 | 11,000 | 11,000 | 29 | 1,576 | 3,899,392 |
| 50,000 | 61,000 | 61,000 | 158 | 8,855 | 19,976,192 |

The measurements grow one database cumulatively. Read time tracks cumulative global ledger size; insertion time is the aggregate elapsed time for each newly-added batch. These characterize scan growth, not a clean per-row linear regression model.
## Alternatives
1. **No data change:** retain the existing default-library complement scan. It preserves the current authority model and requires no migration or write-path changes, but reads scale with the entire ledger, including unrelated rows.

2. **Derived `browse_ledger_owners` index (recommended):** derive candidate ownership from authoritative paths and Library configuration; do not elevate the index into a source of truth or alter ADR-0008's authority boundary. Tie rows to an ownership revision and reject stale generations. Build in batches, publish by generation swap only after completeness/revision validation, and recover by discarding incomplete generations and rebuilding. Keep a safe rollback to the current query when the derived generation is absent or invalid. Measure write/storage overhead before approval. Deletion and Transfer continue to use strict-fresh ownership semantics. A4 MUST NOT depend on this unapproved interface.
3. **Persistent `root_id` facts:** store ownership/root association on ledger rows and update it on path/root changes. This can make lookup direct, but turns derived ownership into persistent row facts with broader migration, update, ambiguity, and recovery obligations; it risks conflating an optimization with authoritative ownership.

Recommendation: approve the derived index design for a separate implementation task, with generation/revision safety and rollback specified above. This design gate makes no approval decision for the user and does not claim DB-07 fixed. ADR-0008 remains authoritative unchanged.

## Approval

Approved for implementation: avoid full-table scans for every library kind and
add the supporting index rather than changing stored root ids. Implementation
slices (each with its own migration, tests and fallback):

1. persisted derived table `browse_ledger_owners(ledger_id, library_id, ownership_revision)` plus an ownership revision counter;
2. batched initial build with a generation swap and crash recovery (a stale/incomplete generation is never used; the reader falls back to the current bounded scan);
3. reader switch for `ledger_candidates_for_library`, with the default-library fallback preserved and `browse_owned_ledger_ids` returning `None` while the index is not ready;
4. deletion/transfer keep strict-fresh semantics; rollback stops new dispatch and drains without deleting queued facts.

