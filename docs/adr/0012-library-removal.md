# ADR-0012: Library removal preserves files and canonical Playback identity

- Status: Accepted design; implementation acceptance is tracked separately below.
- Scope: approved Library deletion and NFO reimport plan, Tasks 1–7.

## Context

A Library configuration, its ledger/file-derived facts, a canonical Media, and a User's Playback history have different lifetimes. Removing a configured root must not mean deleting disk content or forgetting the Media identity used by Playback. Recreating a root creates a new Library and new ledger rows; matching only a filename or reusing an old path is not sufficient to recover identity safely.

Store uses four independent SQLite connections/files. An ordinary transaction on one connection does not atomically commit all four databases. Cancellation notification alone also does not prove an old worker has stopped publishing: blocking scans and child processes can outlive the requesting future.

## Decision

### Remove indexing facts, not disk content or user history

Removal deletes only the strict ownership snapshot's ledger and derived file metadata, fingerprint caches/samples/attempts and probe-stage state. It retains Media and each User's Playback position, watched state and favorite state, as well as Subscribe, Downloader configuration and Catalog caches. Video, STRM, subtitles, NFO, poster, fanart and episode artwork are neither removed, moved nor rewritten by this operation.

Ownership is captured before roots disappear, using the longest uniquely matching root and directory-component boundaries. Default-library fallback is not deletion ownership. Ambiguous roots fail preflight rather than guessing. Nested independent libraries and another Library's file for the same Media remain intact. Shared Media/episode facts cannot be indiscriminately cleared while another ledger still needs them.

The last Library of a kind remains protected under the existing default-Library/Transfer contract. Removing a non-last default promotes an existing Library only at finalization. Supporting zero libraries of a kind is a separate change.

### Durable operation and publication barriers

Admin `DELETE /api/v1/libraries/{id}` returns `202` with an operation identifier, including when cleanup is fast. Admin `GET /api/v1/library-removals/{operation_id}` exposes `frozen`, `draining`, `cleaning`, `succeeded` or `failed`, the snapshot removal count and any error. Repeated requests use the existing operation.

The sequence is freeze/snapshot → prevent new work → cancel queued/running work → wait for leases to drain → idempotent scoped cleanup → delete root configuration → succeeded. Registry/Store locking and publication checks must prevent a canceled worker from writing ledger data, probe facts or sidecars, including after another Library reuses the same root. Child-process work must actually terminate and be reaped. A drain timeout fails the operation and leaves the Library frozen; it is not reported as success.

The durable operation stores the cleanup scope so a restart or partial cross-database cleanup can resume safely. It does not pretend the four databases share a transaction. Cleanup retries must preserve the snapshot count and not revive canceled work. Frozen libraries are hidden from ordinary browsing/new Playback and remain visible for administrative status/retry. Static artwork read endpoints remain public under the existing image-authentication contract.

Explicit Subscribe bindings to a removed Library remain bindings with an actionable unavailable-target state. They are not silently redirected to a default Library, and the Subscribe itself is not deleted.

### Reimport identity comes from reliable work-level aliases

Reimport creates new ledger identifiers and reuses an existing MediaId only when the work identity is reliable. External aliases include MediaKind: equal numeric TMDB movie and TV identifiers do not merge works. Conflicting provider aliases must fail rather than choose an arbitrary Media.

Movie identity comes from the matching movie NFO. TV identity comes from bounded ancestor `tvshow.nfo` lookup inside the Library root. An `episodedetails` NFO can supply season/episode facts, but its episode title/identifier is not a TV Media identity. Valid NFO identity and display metadata take precedence over a confident filename. Title-only fallback is allowed only when unique and non-conflicting; absent or ambiguous identifiers do not guarantee history recovery. Old history is retained even when it cannot safely be associated with a newly scanned file.

A complete local identity plus display title avoids implicit Catalog search/details and existing-sidecar rewrites; an explicit refresh may still fetch fresh metadata. A later online identification must converge the new ledger on the canonical MediaId before publication rather than attach the same alias to a competing Media.

NFO metadata is not stream-probe output. Reimport must schedule fresh media probing for the new ledger rather than copy old file_meta or infer probe completion from NFO runtime/streamdetails. New libraries keep `detect_intros=false` and `enable_fingerprint=false`; these switches do not disable basic media probing.

## Consequences and acceptance boundaries

- Reliable MediaId reuse reconnects each User's independent Playback facts without migrating progress by path.
- Files stay available to external tools throughout removal. The application must stop offering new Playback through removed ledger rows.
- Failed removal may require retry and stays frozen; this is preferable to apparent success with a live publisher.
- No path-cache migration, unowned-ledger adoption or history-clearing option is introduced.

The public API regression in `crates/api/tests/library_delete_reimport.rs` covers TV S01E03 positions 750000/120000 ms with distinct favorite/watched states, whole-movie positions, fresh ledger identity, old file-meta/fingerprint/stage cleanup, preserved sidecar bytes, an independent Library's same-Media file/cache, default-off detection switches, and a different-alias negative case. It constructs ApiState before entering Tokio so probe workers are not started, and injects an offline Catalog. This checks cache non-reuse, **not** successful fresh probe execution, real streaming or child-process cancellation. Creation also schedules an automatic scan; the explicit public scan is intentionally idempotent with that path.

Restart-mid-clean, restart-draining, canceled-job recovery, lease/publication races, ambiguous no-ID reimport, nested-root ownership, member visibility, Subscribe-target handling and actual fake-probe completion require the corresponding focused suites and/or UI regression cases. This ADR is not proof those checks passed. Browser/VidHub cases remain Not run unless actually executed.

## External convention references (limited claim)

The approved plan cites these user-facing removal discussions:

- [Jellyfin: delete a library without deleting files](https://forum.jellyfin.org/t-delete-a-library-without-deleting-files?pid=71881&mode=linear)
- [Emby: delete library](https://emby.media/community/topic/117044-delete-library/)

They are reference leads for the narrow distinction between removing a library configuration and deleting media files, not evidence about either project's internal caches, cancellation or Playback identity retention. During this implementation both URLs could not be fetched because the tool resolved their hosts to non-public addresses. Their contents were therefore **not independently verified** here; no stronger external compatibility claim is made.
