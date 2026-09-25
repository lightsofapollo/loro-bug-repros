# Loro crash repros

Two standalone reproductions for the `loro` Rust crate (and one for `loro-crdt` on npm).
The only dependency is `loro` from crates.io. No `unsafe`.

| | Command | Result on 1.13.9 / 1.16.0 / 1.16.2 / main@ad5b2a6d |
|---|---|---|
| Bug 1: tree time travel overflows the stack | `cargo run -p bug1-tree-overflow` | 7 APIs abort (SIGABRT, stack overflow) |
| Bug 2: conflicting op ids panic inside `import`, then abort | `cargo run -p bug2-import-abort` | panic in `import`, then a second panic in `LoroDoc::drop` (poisoned `LoroMutex`); abort if the drop happens during unwinding |
| Bug 1 in JS (`loro-crdt` 1.16.3) | `cd js-bug1 && bun install && bun repro.ts` | `RuntimeError: Out of bounds memory access`, and the WASM instance stays broken |
| Version matrix | `scripts/matrix.sh [--release]` | both bugs on every version |
| Recursion backtrace (macOS, lldb) | `scripts/backtrace.sh [probe]` | `is_parent_deleted` recursing without end |

Each binary runs every probe in a child process, because a stack overflow or a
double panic kills the whole process. To run one probe in-process:
`cargo run -p bug1-tree-overflow -- fork_at`, `cargo run -p bug2-import-abort -- minimal doc-inside`.

Tested with rustc 1.97.0 on macOS arm64. Versions tested: 1.13.9, 1.16.0 and 1.16.2 from crates.io, and
`main` at `ad5b2a6d4546473d9f4a96412d2ae6808c8403ec` (loro-dev/loro HEAD on 2026-09-24) as a git dependency.

> **Matrix pinning note.** `loro = "=1.13.9"` alone still builds against **loro-internal 1.16.2**:
> loro's dependencies on `loro-internal`, `loro-common` and `loro-kv-store` are caret requirements.
> `scripts/matrix.sh` pins them in the lockfile (`cargo update -p loro-internal --precise 1.13.9` and so on) and prints
> the resolved versions, so each row of the matrix really runs that version's internals.

---

## Bug 1: stack overflow when time-travelling a `LoroTree` after concurrent moves

### Minimal scenario: 2 peers, 2 nodes, 6 ops

```rust
let a = LoroDoc::new(); a.set_peer_id(1)?;
let b = LoroDoc::new(); b.set_peer_id(2)?;
let (ta, tb) = (a.get_tree("tree"), b.get_tree("tree"));

let n0 = ta.create(TreeParentId::Root)?;          // A@0
let n1 = ta.create(TreeParentId::Root)?;          // A@1
a.commit();
b.import(&a.export(ExportMode::all_updates())?)?;

ta.mov(n1, TreeParentId::Node(n0))?; a.commit();  // A@2  lamport 2   n1 under n0
let a_upto_2 = a.export(ExportMode::all_updates())?;

tb.mov(n0, TreeParentId::Root)?;     b.commit();  // B@0  lamport 2   only raises B's lamport
tb.mov(n0, TreeParentId::Node(n1))?; b.commit();  // B@1  lamport 3   n0 under n1
b.import(&a_upto_2)?;                             // B is now at [A@2, B@1]

ta.mov(n1, TreeParentId::Root)?;     a.commit();  // A@3  lamport 3   n1 back to root
a.import(&b.export(ExportMode::all_updates())?)?;

let target = Frontiers::from(vec![ID::new(1, 2), ID::new(2, 1)]); // == b.state_frontiers() above
a.fork_at(&target);   // stack overflow, process aborts
```

The target is not contrived: it is exactly `b.state_frontiers()` right after B imported A's first three ops.

### Expected state at `[A@2, B@1]`

Ops in (lamport, peer) order: A@0, A@1 (create), A@2 `n1 -> n0` (applies), B@0 `n0 -> root` (applies),
B@1 `n0 -> n1` (would make a cycle, so it is ignored). **Expected: `root { n0 { n1 } }`.**

Checked three independent ways:

| Method | Result |
|---|---|
| By hand (above) | `root { n0 { n1 } }` |
| B's live tree when B was at that version | `root { n0 { n1 } }` |
| `export(ExportMode::updates_till(&vv))` into a fresh doc | `root { n0 { n1 } }` |

(The final state with all ops is `root { n1 { n0 } }`: A@3 sorts before B@1, so B@1 applies.)

### Actual (`cargo run -p bug1-tree-overflow`, identical on all four versions)

```
final tree (all ops)          : root { n1 { n0 } }
target frontier               : Frontiers([1@2, 2@1])
expected at target (by hand)  : root { n0 { n1 } }
B's live tree at target       : root { n0 { n1 } }

fork_at: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
checkout: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
revert_to: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
diff_latest_to_target: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
diff_target_to_latest: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
export_snapshot_at: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
export_state_only_at: root { n0 { n1 } }
export_shallow_snapshot_at: root { n1 { n0 } }
shallow_snapshot_then_checkout: STACK OVERFLOW -> process aborted (signal: 6 (SIGABRT))
undo: undo_count=3 undo=true -> root {  } (parent(n0)=Some(Node(TreeID { peer: 1, counter: 1 })), parent(n1)=Some(Deleted))
undo_b_followup: undo=true -> root { n0, n1 }
export_updates_till: root { n0 { n1 } }

7 probe(s) crashed
```

- Overflow: `fork_at`, `checkout`, `revert_to`, `diff` (both directions), `export(SnapshotAt)`, and
  `checkout(target)` on a doc imported from `export(ShallowSnapshot(target))`.
- No overflow: `export(StateOnly(Some(target)))` returns the correct tree; `updates_till` is correct;
  `export(ShallowSnapshot(target))` returns the latest state, which is correct for that mode.
- `UndoManager::undo`: **no overflow observed.** See "Differences from the claim".

### Where it recurses

`scripts/backtrace.sh fork_at` (lldb, with a 256 KiB stack so the full trace prints), on 1.16.2:

```
      1 TreeCacheForDiff::get_parent_with_id @ tree.rs:569:40
      1 TreeCacheForDiff::is_parent_deleted  @ tree.rs:557:67
   1177 TreeCacheForDiff::is_parent_deleted  @ tree.rs:557:44
      1 TreeDiffCalculator::checkout_diff::{closure#0} @ tree.rs:301:64
      1 TreeDiffCalculator::checkout_diff @ tree.rs:231:15
      1 TreeDiffCalculator::diff @ tree.rs:160:14
        ... DiffCalculator::calc_diff_internal -> LoroDoc::_checkout_without_emitting
        ... encode_snapshot_at -> export_snapshot_at -> LoroDoc::export -> LoroDoc::fork_at
```

`checkout`, `revert_to`, `diff` and the shallow-snapshot checkout have the same inner stack. With 2 nodes a
finite parent chain is at most 2 deep, so ~1200 frames on a 256 KiB stack means a parent cycle, not a deep tree.

The loop is at `crates/loro-internal/src/diff_calc/tree.rs:301`, the retreat pass of `checkout_diff`:
`tree_cache.is_parent_deleted(old_parent)`.

### Root cause (from reading the source; consistent with every experiment here)

`TreeCacheForDiff` stores each move op with an `effected` flag. The flag is computed once, when the op is
applied in (lamport, peer) order, against whatever ops came before it at that time.

`checkout_diff` goes back to the target by retreating ops that are **not in the target's version vector**.
The retreated set is not a suffix of the (lamport, peer) order: A@3 (3,1) is retreated, but B@1 (3,2) sorts
after it and stays, because it is in the target. B@1 keeps `effected = true`, which it only earned because
A@3 had already moved n1 back to root. Without A@3, the cache now holds n1's parent = n0 (A@2) and n0's parent = n1 (B@1):
a cycle. `is_parent_deleted` walks parents with no cycle guard and recurses forever. `is_ancestor_of` has only a
self-parent guard and walks the same data. The earlier `checkout` pass in the same file retreats the same way.

Evidence that this is the mechanism: in a 3-node variant of this scenario, removing B's lamport-raising op
(so B's cycle move sorts before A's move back to root) makes the crash disappear. A fuzzer over random 2-peer, 3-node move sequences (checking `fork_at` at every frontier either peer
passed through) crashed quickly on multi-head frontiers. In 15,000 cases it never crashed when restricted to
single-head frontiers, presumably because the replay base for those lands below the concurrency.

A fix probably needs the retreat to recompute `effected` for ops that stay and sort after a retreated op, or
to retreat to a point below the lowest retreated op and replay forward. At minimum, add a cycle guard to
`is_parent_deleted` and `is_ancestor_of` so the result is a wrong answer or an `Err`, not an abort.

### JS (`loro-crdt` 1.16.3, the latest on npm, newer than crates.io's 1.16.2)

```
$ cd js-bug1 && bun install && bun repro.ts
loro-crdt 1.16.3
final tree: root { n1 { n0 } }; expected at target: root { n0 { n1 } }
forkAt: THREW RuntimeError: Out of bounds memory access (evaluating 'wasm.callPendingEvents()')
   new LoroDoc() afterwards: THREW Out of bounds memory access (evaluating 'wasm.lorodoc_new()')
checkout: THREW RuntimeError: Out of bounds memory access ...
revertTo: THREW RuntimeError: Out of bounds memory access ...
diff: THREW RuntimeError: Out of bounds memory access ...
updatesTill: root { n0 { n1 } }
```

In WASM the Rust stack lives in linear memory with no guard page, so the runaway recursion surfaces as an
out-of-bounds trap. After that trap **every** later call into the module fails, including `new LoroDoc()`, so one bad
`forkAt` breaks every document in the page or process. (JS has no `snapshot-at` export mode.)

---

## Bug 2: importing an update that reuses known op ids panics, poisons the doc, and can abort

Loro's docs warn that reusing a PeerID across concurrent writers "can produce conflicting OpIDs and corrupt
the document", so wrong data is arguably expected here. A panic, followed by a process abort from inside
`import`, on well-formed bytes from the network, is not.

### Fixture-free minimal repro (`bug2-import-abort/src/main.rs`, `minimal()`)

```rust
let history = LoroDoc::new();
history.set_peer_id(7)?;
history.get_map("m").insert("k", 0)?;        // 7@0
history.get_text("t").insert(0, "a")?;       // 7@1
history.commit();

let other = LoroDoc::new();
other.set_peer_id(7)?;                        // same peer id: the client bug
other.get_text("t").insert(0, "xyz")?;       // 7@0..=2
other.commit();

history.import(&other.export(ExportMode::all_updates())?); // panics
```

Loro treats 7@0..=1 as already known, trims the first two chars off the incoming insert, and applies the rest
(7@2, `"z"`) at position `0 + 2 = 2` in a text that has length 1.

- release: `generic-btree-0.10.7/src/lib.rs:618` `elem.rle_len=1 but pos.offset=2` from
  `RichtextState::apply_diff -> insert_elem_at_entity_index -> BTree::insert_by_path -> split_leaf_if_needed`
- debug: an earlier `debug_assert!` fires first, at `loro-internal/src/container/richtext/richtext_state.rs:1606`
  (`entity_index=2 len=1`; line 1589 in 1.13.9)

The first panic happens while `import` holds the doc's transaction mutex. `LoroDoc::drop` calls
`commit_internal`, which locks that mutex and panics with `poisoned LoroMutex` (`loro-internal/src/sync.rs:34`).

### Actual (`cargo run --release -p bug2-import-abort`)

| Case | What the caller does | Outcome |
|---|---|---|
| `doc-inside` | `catch_unwind(\|\| { let doc = ...; doc.import(&u) })` | first panic, then `poisoned LoroMutex` in `Drop` during unwinding, then `panic in a destructor during cleanup` and **SIGABRT** |
| `doc-outside` | `catch_unwind(AssertUnwindSafe(\|\| doc.import(&u)))`, then `drop(doc)` | `catch_unwind` **does catch** the first panic; the later `drop(doc)` panics (`poisoned LoroMutex`), exit 101 |
| `doc-outside-forget` | as above, then `mem::forget(doc)` | survives, but the doc is unusable: every call panics |

The same happens with the reporters' fixtures (`fixture` case): release panics with exactly the reported
`generic-btree lib.rs:618 elem.rle_len=1 but pos.offset=3` (element `Style { key: "em", ... }`). The fixture
update is peer `2918796465566129979`'s ops 0..=16 (a 7-char text insert `" agent2"`, another insert, and
three map ops), while the history already has that peer's ops 0..=2 as three unrelated map inserts.

Identical on 1.13.9, 1.16.0, 1.16.2 and main@ad5b2a6d, in both profiles.

Related silent behaviour seen while minimizing: when the trimmed position happens to be in range, `import`
returns `Ok` and silently applies a sliced op at a shifted position (for example `"hello world"` became
`"helabclo wordefgld"`). The panic is the out-of-range special case of a conflict that is never detected.

---

## Differences from the claims

**Bug 1: confirmed, with corrections**
- Smaller than claimed: 2 nodes, 2 peers, 6 ops (claimed 3 nodes, about 7). I could not go lower: the rejected move
  must sort after a later op from the other peer, which needs one op to raise its lamport.
- Recursion site: my backtraces show only `is_parent_deleted` (retreat pass, `tree.rs:301`).
  `is_ancestor_of` has the same unguarded walk, but I never saw it on the stack.
- Also affected but not claimed: `export(StateOnly)` does **not** overflow; `checkout` on a doc imported from a
  shallow snapshot **does** overflow.
- `UndoManager::undo`: **not reproduced.** A's undo and B's undo of a follow-up op whose deps are the bad version
  both return without overflow. Undo-all fuzzing found nothing (25,000 random 2-peer move sequences, undoing every step on each peer).
  `undo()` only diffs between single-op frontiers, which do not seem to reach the bad retreat. A's undo does
  something questionable, though: undoing `n1 -> root` leaves n1 **deleted** and n0 under it, so both nodes
  vanish from the tree. Possibly related to open issue #1055. I did not investigate.
- The version claim holds, but see the pinning note: `loro = "=1.13.9"` without lockfile pins actually tests
  loro-internal 1.16.2.

**Bug 2: confirmed, but overstated**
- The panic site and message match the claim exactly (release). In debug builds a `debug_assert!` in
  `richtext_state.rs` fires first.
- "catch_unwind cannot contain it" is **not accurate in general**. `catch_unwind` catches the first panic. The
  abort happens only when the `LoroDoc` is dropped during the unwind (the doc is owned inside the closure, or
  the panic is not caught). Either way the doc is poisoned: every later call panics, including `Drop`, so it
  has to be leaked to survive.
- A fixture-free repro exists: 3 API calls, 5 op ids.

## Related upstream reports (read-only search, 2026-09-24)

No existing report of either bug found (searched: stack overflow, is_parent_deleted, is_ancestor_of, tree
move/cycle/checkout/revert, rle_len, split_leaf_if_needed, same/duplicate peer id, poisoned, abort, catch_unwind).
Related:
- #1068 (open): `import()` panics on updates depending on shallow-folded ops, and "the panic poisons the doc
  mutex" then aborts in a destructor. Same poison-then-abort mechanism as bug 2, different trigger.
  #1080 (closed) and #1083 (open PR) fix that trigger by returning `Err`, not the mechanism.
- #1106 (closed) / #1107: `fork_at` / historical diff aborting for movable lists. Same family of APIs, different container.
- #1055 (open): undoing a post-import `LoroTree` edit deletes the pre-existing node. Possibly related to the undo observation above.
- #1071, #1084: replay-base selection for checkout (the `find_replay_base` window that `checkout_diff` relies on).
- #957 (closed): an earlier `generic-btree` assertion during import, fixed by #952's atomic rollback.
- #1115 (open PR, 2026-09-23): `fork_at` / snapshot-at on shallow docs. It touches the same entry points; I did not check whether it changes this bug.

## Layout

```
Cargo.toml                   workspace; `loro` version in [workspace.dependencies]
bug1-tree-overflow/          bug 1 probes
bug2-import-abort/           bug 2 cases + fixtures/ (copied from the reporters, read-only originals untouched)
js-bug1/                     bug 1 against loro-crdt (bun)
scripts/matrix.sh            runs both repros across versions (matrix/<version>/*.log)
scripts/backtrace.sh         collapsed lldb backtrace for a bug 1 probe
```
