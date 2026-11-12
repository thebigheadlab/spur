# 3. Text model and buffer API

- Status: proposed
- Date: 2026-11-07
- Spec: #52 (text engine), #4; architecture #3. Related tasks: #17, #18, #20, #22. (Issue numbers refer to the project's spec issues, which are being restored.)

## Context

Spur runs one core state thread that orders every edit (#3). Frontends are thin clients with no replica in M0-M3. Real-time collaboration is not a v1 goal; shared sessions (M4) use the same server.

The text engine must support:

- async edits from agents and extensions that were computed against an older version;
- per-author undo ("undo mine" never touches someone else's text), and per window within an author (amended by ADR 0006);
- anchors that survive edits (diagnostics, agent proposals, bookmarks);
- cheap snapshots for background work (highlighting, search, agents);
- byte-exact round trip of line endings.

Reused crates must be MIT/Apache-2.0 compatible and on crates.io. `unsafe_code` is denied in Spur crates.

This ADR covers questions Q0, Q1, Q3, Q6 and Q7 of #52 and the public API. Large files, the journal and buffer identity (Q2, Q4, Q5) are ADR 0004.

## Decisions

### Q0 Concurrency model

Each buffer has one authoritative sequencer: the core state thread. Every committed transaction gets the next `Version` (a `u64`, starting at 0 for the loaded text). Versions are per buffer and never reused. There are no clocks, replica ids or fragment ids in the API.

### Q1 Rope: ropey 2 (beta), behind a private module

The rope is used only inside `spur-text` (`storage` module). No rope type appears in the public API, so the crate can be swapped without touching callers.

Spike (seeded synthetic text, 100k single-byte inserts at random offsets, release build, 4 vCPU cloud container; numbers are indicative, the reference-machine run is part of #33):

| 100 MB ASCII | load | memory | insert p50 | insert p99 | clone | byte to line + UTF-16 |
|---|---|---|---|---|---|---|
| ropey 1.6.1 | 143 ms | 1.14x | 1.95 µs | 34.0 µs | 72 ns | 1.19 µs |
| ropey 2.0.0-beta.1 | 61 ms | 1.10x | 0.89 µs | 3.35 µs | 128 ns | 1.29 µs |
| crop 0.4.3 | 30 ms | 1.04x | 0.63 µs | 3.15 µs | 173 ns | n/a (no UTF-16 metric) |

- **Chosen: ropey 2.0.0-beta.1**, pinned with `=`. Byte-indexed API (no char conversions on every edit), built-in LF/CR/CRLF line metric, UTF-16 metric for LSP, CRLF never split across chunks, O(1) `Arc` clone, MIT OR Apache-2.0.
- Rejected: **ropey 1.6** (char-indexed edits, 10x worse p99, Unicode line breaks unless disabled); **crop** (fastest, but no UTF-16 metric and no CR-only lines; we would add both); **own SumTree** (research recommendation for sharing one tree with the display map and anchors; deferred until #20 shows it is needed, the private module keeps that door open).
- Risk: beta crate. Mitigation: exact pin, property tests against a `String` model, and the private module so falling back to ropey 1 or crop is a local change.

The public read API is range based: `len`, `slice(range)`, `chunks(range)`, coordinate conversions and line access. There is deliberately no "whole text as `String`" call on the hot path, so a large-file backend (ADR 0004) can be added behind the same API.

### Q3 Edit model: ChangeSet transactions rebased by the sequencer

Decided by the owner on 2026-11-07.

- A `ChangeSet` is a sorted list of non-overlapping `Change { range, text }` hunks in byte offsets of one base text. Touching hunks are merged, so the form is canonical.
- A `Transaction` is `{ author, base, changes, selection_before, selection_after, group }`.
- If `base` is the current version, the changes apply as is.
- If `base` is older, the sequencer transforms the changes through every committed change set since `base`, in order:
  - A hunk that overlaps a committed hunk (positive-length intersection, or an insertion strictly inside the other's replaced range) is a conflict. The whole transaction is rejected with `EditError::Stale` and the list of conflicts. Nothing is applied partially.
  - Otherwise each hunk is mapped. Inserts at the same offset are ordered deterministically: already committed text comes first. This includes an insert at the start of a committed replacement: it lands after the replacement text. So if a user types just before a word that an agent rewrote, the user's text ends up after the rewritten word.
  - If `base` is older than the retained change log, the result is `EditError::TooOld`.
- The change log keeps the last `N` change sets (default 10 000) and at most 64 MB of inserted text (default), whichever limit is hit first, so whole-file rewrites cannot grow it without bound. Both limits are set per buffer (`Buffer::with_log_limits`). Rebasing and lazy anchors work only inside that window.
- Rejected: **fragment CRDT per buffer** (Zed) and **hybrid** (eg-walker, diamond-types). With one sequencer and no replicas, a CRDT adds identity metadata on every character without buying anything. Reopen condition: client replicas, offline merge or host-less collaboration. Then adopt a crate (cola, diamond-types, loro, yrs) behind the same API.

### Anchors

Two kinds, because #52's "resolve 10k anchors after 10k edits" cannot be met by mapping each anchor through 10k change sets on demand:

- **`Anchor { version, offset, bias }`**: a value type. It resolves lazily by mapping through the change log since `version`. Cheap to create, good for a few positions (an agent's target, a jump location). Fails with `TooOld` outside the log window.
- **Tracked anchors** (`AnchorId`): registered in the buffer and updated eagerly on every commit (one pass over the live anchors per transaction). Resolving is O(1). Used for large sets (diagnostics, highlights, review hunks). Ids carry a generation, so a removed id never resolves to a reused slot.

Tracked anchors cost O(live anchors) per commit (about 65 µs for 10k anchors on the spike machine). If #20 needs far more, they move into a SumTree keyed by offset; the `AnchorId` API stays.

Mapping rule, shared by anchors, selections and rebase: a position before a hunk is unchanged; after a hunk it shifts by the hunk's length delta; a position touching or inside a replaced range snaps to the start of the replacement (`Bias::Left`) or its end (`Bias::Right`).

### Q6 Undo model

- History is an undo tree: a flat `Vec<Revision>` with parent and last-child links (Helix). Each revision stores its author, group, change set, inverse, selections before and after, the version that committed it, and a wall-clock timestamp (for "earlier by duration" and "undo to last save").
- `undo(author)`:
  - If the head revision is the author's, move the head to its parent by applying the inverse. Redo follows the tree.
  - Otherwise find the author's latest revision on the path from the root to the head that is still in effect. Undo revisions are skipped. A redo revision (one that reverts an undo) is a target itself, so undo, redo, undo works even after other authors edited in between. Rebase its inverse through every later revision. On overlap with another author's later edit, refuse with `UndoError::Conflict` and the conflicting ranges; never undo partially. On success, commit the rebased inverse as a new revision that records which revision it reverts.
- `redo(author)` replays the author's own undo stack: a tree move if the author's undo was a tree move and the head has not moved since, otherwise it reverts the revert.
- Lanes (added by ADR 0006): a transaction may name an undo lane (a window). Undo and redo take an `UndoScope` (author plus lane) and touch only revisions made in that scope, so everything above that says "author" means "scope". An undo moves the head only when no other scope has a redo that needs the head on the path; otherwise it reverts. When a redo has to be rebased, the scope's later redos are rewritten to follow the new revision.
- Groups: consecutive transactions with the same author, lane and `GroupKey` (actor plus turn id, for example one insert-mode session or one agent turn) merge into one revision, so one undo step undoes the whole group. A transaction from another author in between ends the group.
- Finding the target walks the head path, and a revert rebases over every later revision: O(revisions) work. That is fine for M0. The history cap from #18 (default 10 000 revisions or 50 MB) also bounds this walk.
- Every undo and redo is itself a committed transaction with a new `Version`, so anchors, the change log and rebasing see one uniform stream of edits.

### Q7 Text representation

- Internal text is UTF-8.
- Line endings are stored verbatim (LF, CRLF, CR, mixed) and never normalized. Line indexing counts LF, CRLF and CR as breaks; CRLF is one break and is never split by the rope.
- Decoding other encodings and preserving undecodable bytes happens at load and save (file IO, #19, ADR 0004). The engine itself only holds valid UTF-8. Edits must start and end on char boundaries; invalid offsets are rejected with an error, never a panic.

## API (crate `spur-text`)

```rust
pub struct Version(u64);
pub enum AuthorId { User, Extension(Arc<str>), Agent(Arc<str>), Rpc(Arc<str>), External(Arc<str>) }
pub struct GroupKey { actor: AuthorId, turn: u64 }
pub enum Bias { Left, Right }

pub struct Change { pub range: Range<usize>, pub text: Arc<str> }
pub struct ChangeSet { /* sorted, non-overlapping hunks + base length */ }
impl ChangeSet {
    pub fn new(base_len: usize, changes: impl IntoIterator<Item = Change>) -> Result<Self, ChangeError>;
    pub fn insert(base_len, at, text) -> Self; pub fn delete(base_len, range) -> Self; pub fn replace(...) -> Self;
    pub fn map_offset(&self, offset: usize, bias: Bias) -> usize;
    pub fn compose(&self, next: &ChangeSet) -> ChangeSet;
    pub fn rebase(&self, over: &ChangeSet) -> Result<ChangeSet, Vec<HunkConflict>>;
}

pub struct Selection { pub ranges: Vec<SelRange>, pub primary: usize }
pub struct Transaction { pub author, pub base: Version, pub changes: ChangeSet,
    pub selection_before: Option<Selection>, pub selection_after: Option<Selection>,
    pub lane: Option<UndoLane>, pub group: Option<GroupKey> }
pub struct UndoScope { pub author: AuthorId, pub lane: Option<UndoLane> }  // From<AuthorId>: lane None

pub struct Snapshot { /* O(1) clone, Send + Sync */ }
impl Snapshot {
    pub fn version(&self) -> Version; pub fn len(&self) -> usize;
    pub fn slice(&self, range) -> String; pub fn chunks(&self, range) -> impl Iterator<Item = &str>;
    pub fn line_count(&self) -> usize; pub fn line_range(&self, line) -> Range<usize>;
    // index units: byte (canonical), char, UTF-16, Point { line, column (bytes) }, graphemes
    pub fn byte_to_char / char_to_byte / byte_to_utf16 / utf16_to_byte / byte_to_point / point_to_byte;
    pub fn next_grapheme_boundary / prev_grapheme_boundary;
}

pub struct Buffer { /* rope, version, change log, undo tree, tracked anchors */ }
impl Buffer {
    pub fn new(text: &str) -> Self;
    pub fn snapshot(&self) -> Snapshot; pub fn version(&self) -> Version;
    pub fn apply(&mut self, txn: Transaction) -> Result<Applied, EditError>;
    // EditError: Stale | TooOld | FutureVersion | LengthMismatch | NotCharBoundary
    pub fn undo(&mut self, scope: &UndoScope) -> Result<Option<Applied>, UndoError>;
    pub fn redo(&mut self, scope: &UndoScope) -> Result<Option<Applied>, UndoError>;
    pub fn forget_redo(&mut self, scope: &UndoScope);
    pub fn anchor(&self, offset, bias) -> Anchor; pub fn resolve(&self, anchor) -> Result<usize, AnchorError>;
    pub fn with_log_limits(text: &str, capacity: usize, byte_limit: usize) -> Self;
    pub fn last_edit_time(&self) -> Option<SystemTime>;
    pub fn track(&mut self, offset, bias) -> AnchorId; pub fn tracked(&self, id) -> Option<usize>; pub fn untrack(&mut self, id);
}
```

Conflicts (`Conflict`) report the incoming hunk in the coordinates it was submitted in, for both `apply` and `undo`.

A rebased transaction keeps its before-selection, mapped through the edits since its base, so undoing it restores the cursor. Its after-selection is dropped, because it refers to text the author never saw.

`Applied` reports the new version, the change set as actually applied (after rebase) and the resulting selection, so frontends and extensions get the same edit stream.

Ownership: `spur-text` owns everything above. Views, cursors per view, multibuffers and commands live in `spur-core` (#20) and use `Snapshot`, `Anchor` and `Selection` from here. `AuthorId` mirrors the #3 actor (user, extension, agent, RPC client) plus `External` for reloads and formatters.

## Evaluation

Criterion benches in `crates/spur-text/benches/` with seeded generated text (never committed). Thresholds from #52:

| Measure | Threshold |
|---|---|
| Insert p99, 100 MB, full transaction (anchors + undo) | < 1 ms (journal append lands with ADR 0004) |
| Rebase a 500-hunk agent change set over 50 user edits | < 2 ms |
| Resolve 10k tracked anchors after 10k edits | < 1 ms |
| Snapshot clone | O(1), < 1 µs |

First results (same 4 vCPU container as the spike, `cargo bench -p spur-text`):

| Measure | Result |
|---|---|
| Insert, 100 MB, 10k tracked anchors, undo on | p50 65 µs, p99 115 µs (anchor updates dominate) |
| Rebase + apply 500-hunk agent change set over 50 user edits (1 MB) | 1.13 ms |
| Resolve 10k tracked anchors after 10k edits | 10.9 µs |
| Snapshot clone, 100 MB | 17 ns |

Not done yet, tracked for #17, #18 and ADR 0004:
- memory after 10k whole-file rewrites: the change log is bounded, but the undo tree is not capped yet (#18: default 10 000 revisions or 50 MB);
- the longest-line metric, and benches at 1 MB, 1 GB and on a single giant line (#17).

Property tests: rebasing equals sequential application when there is no conflict; "undo mine" never changes another author's text; undo then redo round-trips, for one author and for two authors with interleaved edits; tracked and lazy anchors agree.

## Consequences

- One edit stream (`Applied`) for frontends, extensions, agents and the journal.
- Agents can compute large edits against a snapshot off-thread and commit later; conflicts are explicit and deterministic.
- No CRDT metadata cost per character. Collaboration without a server would need a new ADR.
- A beta dependency in the core, contained by a private module and tests.
- Sibling specs to update when GitHub access returns: #4 (no Lamport timestamps, no mmap, external revisions instead of a 3-way merge prompt), #13 (thin client + predictive echo, no local replica), #17, #18, #20, #22.
