//! Bug 1: after two peers make concurrent `LoroTree` moves, time-travel back to
//! the version where one move was rejected as a cycle overflows the stack.
//!
//! Run all probes (each in its own child process, because a stack overflow
//! aborts the whole process):
//!
//!     cargo run -p bug1-tree-overflow
//!
//! Run one probe in-process:
//!
//!     cargo run -p bug1-tree-overflow -- fork_at

use loro::{ExportMode, Frontiers, LoroDoc, LoroTree, TreeID, TreeParentId, UndoManager, ID};
use std::process::{Command, ExitCode};

const PROBES: &[&str] = &[
    "fork_at",
    "checkout",
    "revert_to",
    "diff_latest_to_target",
    "diff_target_to_latest",
    "export_snapshot_at",
    "export_state_only_at",
    "export_shallow_snapshot_at",
    "shallow_snapshot_then_checkout",
    "undo",
    "undo_b_followup",
    "export_updates_till",
];

struct Scenario {
    /// Peer A after merging everything (the document that crashes).
    doc: LoroDoc,
    /// Peer A's undo manager (one step per commit).
    undo: UndoManager,
    /// Peer B after merging everything.
    b: LoroDoc,
    /// Peer B's undo manager (one step per commit).
    b_undo: UndoManager,
    n0: TreeID,
    n1: TreeID,
    /// The version where B's move was rejected: [A@2, B@1].
    target: Frontiers,
    /// B's live tree when B itself was at `target` (independent witness).
    b_live_at_target: String,
}

/// 2 peers, 2 nodes, 6 ops (2 creates + 4 moves).
///
/// ```text
/// A: create n0 (A@0), create n1 (A@1)          -> B imports
/// A: move n1 under n0          (A@2, lamport 2)
/// B: move n0 to root           (B@0, lamport 2)   only bumps B's lamport
/// B: move n0 under n1          (B@1, lamport 3)
/// B imports A@0..=2            -> B is now at [A@2, B@1]; B@1 would make a
///                                 cycle, so it is ignored: root { n0 { n1 } }
/// A: move n1 to root           (A@3, lamport 3)   concurrent with B@1
/// A imports B                  -> final: root { n1 { n0 } }
/// ```
///
/// A@3 sorts before B@1 (same lamport, lower peer), so in the final history
/// B@1 is applied and effective. Going back to [A@2, B@1] must drop A@3,
/// which makes B@1 a cycle again.
///
/// With `b_followup`, B makes one more local move (B@2: n1 -> root) right
/// after reaching [A@2, B@1], so B@2's deps are the bad version. Undoing B@2
/// has to compute the diff back to its deps.
fn scenario(b_followup: bool) -> Scenario {
    let a = LoroDoc::new();
    a.set_peer_id(1).unwrap();
    let b = LoroDoc::new();
    b.set_peer_id(2).unwrap();
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    let mut b_undo = UndoManager::new(&b);
    b_undo.set_merge_interval(0);
    let ta = a.get_tree("tree");
    let tb = b.get_tree("tree");

    let n0 = ta.create(TreeParentId::Root).unwrap();
    let n1 = ta.create(TreeParentId::Root).unwrap();
    a.commit();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();

    ta.mov(n1, TreeParentId::Node(n0)).unwrap();
    a.commit();
    let a_upto_2 = a.export(ExportMode::all_updates()).unwrap();

    tb.mov(n0, TreeParentId::Root).unwrap();
    b.commit();
    tb.mov(n0, TreeParentId::Node(n1)).unwrap();
    b.commit();

    b.import(&a_upto_2).unwrap();
    let target = Frontiers::from(vec![ID::new(1, 2), ID::new(2, 1)]);
    assert_eq!(b.state_frontiers(), target, "B should be exactly at the target");
    let b_live_at_target = render(&tb, n0, n1);
    if b_followup {
        tb.mov(n1, TreeParentId::Root).unwrap();
        b.commit();
    }

    ta.mov(n1, TreeParentId::Root).unwrap();
    a.commit();
    a.import(&b.export(ExportMode::all_updates()).unwrap()).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();

    Scenario { doc: a, undo, b, b_undo, n0, n1, target, b_live_at_target }
}

fn render(tree: &LoroTree, n0: TreeID, n1: TreeID) -> String {
    fn name(id: TreeID, n0: TreeID, n1: TreeID) -> &'static str {
        if id == n0 {
            "n0"
        } else if id == n1 {
            "n1"
        } else {
            "?"
        }
    }
    fn walk(tree: &LoroTree, parent: TreeParentId, n0: TreeID, n1: TreeID) -> String {
        let kids = tree.children(parent).unwrap_or_default();
        kids.iter()
            .map(|k| {
                let inner = walk(tree, TreeParentId::Node(*k), n0, n1);
                if inner.is_empty() {
                    name(*k, n0, n1).to_string()
                } else {
                    format!("{} {{ {} }}", name(*k, n0, n1), inner)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    format!("root {{ {} }}", walk(tree, TreeParentId::Root, n0, n1))
}

fn doc_tree(doc: &LoroDoc, s: &Scenario) -> String {
    render(&doc.get_tree("tree"), s.n0, s.n1)
}

fn import_into_fresh(bytes: &[u8], s: &Scenario) -> String {
    let d = LoroDoc::new();
    d.import(bytes).unwrap();
    doc_tree(&d, s)
}

fn probe(name: &str) -> String {
    let mut s = scenario(name.ends_with("b_followup"));
    let doc = &s.doc;
    let t = &s.target;
    match name {
        "fork_at" => doc_tree(&doc.fork_at(t).unwrap(), &s),
        "checkout" => {
            doc.checkout(t).unwrap();
            doc_tree(doc, &s)
        }
        "revert_to" => {
            doc.revert_to(t).unwrap();
            doc_tree(doc, &s)
        }
        "diff_latest_to_target" => format!("{} diff item(s)", doc.diff(&doc.oplog_frontiers(), t).unwrap().iter().count()),
        "diff_target_to_latest" => format!("{} diff item(s)", doc.diff(t, &doc.oplog_frontiers()).unwrap().iter().count()),
        "export_snapshot_at" => {
            import_into_fresh(&doc.export(ExportMode::SnapshotAt { version: std::borrow::Cow::Borrowed(t) }).unwrap(), &s)
        }
        "export_state_only_at" => import_into_fresh(&doc.export(ExportMode::state_only(Some(t))).unwrap(), &s),
        // A shallow snapshot carries the *latest* state (history trimmed at `t`),
        // so root { n1 { n0 } } is the correct result here.
        "export_shallow_snapshot_at" => import_into_fresh(&doc.export(ExportMode::shallow_snapshot(t)).unwrap(), &s),
        "shallow_snapshot_then_checkout" => {
            let d = LoroDoc::new();
            d.import(&doc.export(ExportMode::shallow_snapshot(t)).unwrap()).unwrap();
            d.checkout(t).unwrap();
            doc_tree(&d, &s)
        }
        "undo" => {
            // A undoes its last local move (A@3: n1 -> root). Does not overflow:
            // A@3's own versions never include B@1.
            let before = s.undo.undo_count();
            let did = s.undo.undo().unwrap();
            let tree = doc.get_tree("tree");
            format!(
                "undo_count={before} undo={did} -> {} (parent(n0)={:?}, parent(n1)={:?})",
                doc_tree(doc, &s),
                tree.parent(s.n0),
                tree.parent(s.n1)
            )
        }
        "fork_at_b_followup" => doc_tree(&doc.fork_at(&Frontiers::from(ID::new(2, 2))).unwrap(), &s),
        "undo_b_followup" => {
            // 7-op variant: B undoes B@2, whose deps are [A@2, B@1].
            let did = s.b_undo.undo().unwrap();
            format!("undo={did} -> {}", doc_tree(&s.b, &s))
        }
        "export_updates_till" => {
            let vv = doc.frontiers_to_vv(t).unwrap();
            import_into_fresh(&doc.export(ExportMode::updates_till(&vv)).unwrap(), &s)
        }
        other => panic!("unknown probe {other}"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(name) = args.first() {
        println!("{name}: {}", probe(name));
        return ExitCode::SUCCESS;
    }

    let s = scenario(false);
    println!("final tree (all ops)          : {}", doc_tree(&s.doc, &s));
    println!("target frontier               : {:?}", s.target);
    println!("expected at target (by hand)  : root {{ n0 {{ n1 }} }}");
    println!("B's live tree at target       : {}", s.b_live_at_target);
    println!();

    let exe = std::env::current_exe().unwrap();
    let mut failures = 0;
    for p in PROBES {
        let out = Command::new(&exe).arg(p).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let verdict = if out.status.success() {
            stdout.trim().to_string()
        } else if stderr.contains("overflowed its stack") {
            failures += 1;
            format!("{p}: STACK OVERFLOW -> process aborted ({})", out.status)
        } else {
            failures += 1;
            let first = stderr.lines().find(|l| l.contains("panicked") || l.contains("called")).unwrap_or("");
            format!("{p}: FAILED ({}) {first}", out.status)
        };
        println!("{verdict}");
    }
    println!();
    if failures > 0 {
        println!("{failures} probe(s) crashed");
        ExitCode::FAILURE
    } else {
        println!("no probe crashed");
        ExitCode::SUCCESS
    }
}
