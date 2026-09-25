# Loro crash repros

Two small, standalone reproductions for [`loro`](https://github.com/loro-dev/loro), plus one for `loro-crdt` on npm. The only dependency is `loro` from crates.io.

| | Run | What happens |
|---|---|---|
| 1. Time travel on a `LoroTree` after concurrent moves | `cargo run -p bug1-tree-overflow` | `fork_at`, `checkout`, `revert_to`, `diff`, `export(SnapshotAt)` overflow the stack |
| 1 (JS, `loro-crdt` 1.16.3) | `cd js-bug1 && bun install && bun repro.ts` | `RuntimeError: Out of bounds memory access`; the WASM module stays broken afterwards |
| 2. `import` of an update that reuses a peer id | `cargo run -p bug2-import-abort` | panic inside `import`, poisoned doc, abort if dropped while unwinding |
| Both, across versions | `scripts/matrix.sh [--release]` | 1.13.9, 1.16.0, 1.16.2 and `main`@ad5b2a6d |
| Recursion backtrace (macOS, lldb) | `scripts/backtrace.sh [probe]` | |

Each binary runs every probe in a child process, because a stack overflow or an abort ends the process. To run one probe in-process: `cargo run -p bug1-tree-overflow -- fork_at` or `cargo run -p bug2-import-abort -- minimal doc-inside`.

Tested with rustc 1.97.0 on macOS arm64.

Note for older versions: `loro = "=1.13.9"` alone still builds against `loro-internal` 1.16.2, because loro's internal crates are caret dependencies. `scripts/matrix.sh` pins them in the lockfile and prints what it resolved.

---

## 1. Stack overflow when going back in time on a `LoroTree` after concurrent moves

Upstream: [loro-dev/loro#1117](https://github.com/loro-dev/loro/issues/1117)

Two peers, two nodes, six ops:

```rust
use loro::{ExportMode, LoroDoc, TreeParentId::{Node, Root}};

let a = LoroDoc::new(); a.set_peer_id(1)?;
let b = LoroDoc::new(); b.set_peer_id(2)?;
let (ta, tb) = (a.get_tree("tree"), b.get_tree("tree"));

let n0 = ta.create(Root)?;
let n1 = ta.create(Root)?;
a.commit();
b.import(&a.export(ExportMode::all_updates())?)?;

ta.mov(n1, Node(n0))?; a.commit();        // A: n1 under n0
let a_so_far = a.export(ExportMode::all_updates())?;

tb.mov(n0, Root)?;     b.commit();        // B: no-op move (bumps B's lamport)
tb.mov(n0, Node(n1))?; b.commit();        // B: n0 under n1
b.import(&a_so_far)?;
let target = b.state_frontiers();         // [1@2, 2@1]; B's own move is ignored here (cycle)

ta.mov(n1, Root)?;     a.commit();        // A: n1 back to root
a.import(&b.export(ExportMode::all_updates())?)?;

a.fork_at(&target);                       // thread 'main' has overflowed its stack
```

Expected at `target`: `root { n0 { n1 } }`. That is B's live tree when B was at that version, and what `ExportMode::updates_till` into a fresh doc gives.

Output of `cargo run -p bug1-tree-overflow` (same on 1.13.9, 1.16.0, 1.16.2 and `main`@ad5b2a6d):

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
```

(`export_shallow_snapshot_at` carries the latest state, so `root { n1 { n0 } }` is expected there. The `undo` line is included because both nodes disappear from the tree after it; that may be #1055 and is separate from the overflow.)

`scripts/backtrace.sh fork_at` on 1.16.2 shows about 1,200 frames of `TreeCacheForDiff::is_parent_deleted` (`diff_calc/tree.rs:557`), called from `TreeDiffCalculator::checkout_diff` (`tree.rs:301`).

In `loro-crdt` 1.16.3 (`js-bug1/`), `forkAt`, `checkout`, `revertTo` and `diff` throw `RuntimeError: Out of bounds memory access`, and afterwards even `new LoroDoc()` throws.

---

## 2. `import` of an update that reuses a peer id panics and poisons the doc

Upstream: [loro-dev/loro#1118](https://github.com/loro-dev/loro/issues/1118)

Two docs wrongly share a peer id and write different ops:

```rust
use loro::{ExportMode, LoroDoc};

let history = LoroDoc::new();
history.set_peer_id(7)?;
history.get_map("m").insert("k", 0)?;     // 7@0
history.get_text("t").insert(0, "a")?;    // 7@1
history.commit();

let other = LoroDoc::new();
other.set_peer_id(7)?;
other.get_text("t").insert(0, "xyz")?;    // 7@0..=2
other.commit();

history.import(&other.export(ExportMode::all_updates())?); // panics
```

- Release: `generic-btree-0.10.7/src/lib.rs:618` `elem.rle_len=1 but pos.offset=2`, from `RichtextState::apply_diff`.
- Debug: a `debug_assert!` in `loro-internal/src/container/richtext/richtext_state.rs` fires first.
- The doc's mutex is then poisoned, so every later call panics, including `Drop` (`poisoned LoroMutex`, `loro-internal/src/sync.rs:34`).

Output of `cargo run --release -p bug2-import-abort` (same on all four versions):

| Case | What the caller does | Outcome |
|---|---|---|
| `doc-inside` | `catch_unwind(\|\| { let doc = ...; doc.import(&u) })` | panic, then `poisoned LoroMutex` in `Drop` while unwinding: **abort** |
| `doc-outside` | `catch_unwind(AssertUnwindSafe(\|\| doc.import(&u)))`, then `drop(doc)` | first panic is caught; `drop(doc)` panics |
| `doc-outside-forget` | as above, then `mem::forget(doc)` | survives; the doc is unusable |

When the shifted position happens to be valid, `import` returns `Ok` and the text is silently scrambled instead (for example `"hello world"` became `"helabclo wordefgld"`).

The `fixture` case replays the same failure from two small binary files captured from a real app (`bug2-import-abort/fixtures/`).

Loro's docs do say not to reuse peer ids. This repro is about how hard the failure is to recover from.
