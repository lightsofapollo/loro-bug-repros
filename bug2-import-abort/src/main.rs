//! Bug 2: importing an update that reuses already-known op ids (same peer,
//! same counters, different content) panics inside `LoroDoc::import`. The
//! panic poisons the doc's internal `LoroMutex`, and `LoroDoc`'s `Drop` then
//! panics again. If the doc is dropped while the first panic is unwinding,
//! that second panic aborts the process.
//!
//! Run all cases (each in a child process, because an abort kills the process
//! that hits it):
//!
//!     cargo run -p bug2-import-abort
//!
//! Run one case in-process:
//!
//!     cargo run -p bug2-import-abort -- minimal doc-outside
//!     cargo run -p bug2-import-abort -- minimal doc-inside
//!     cargo run -p bug2-import-abort -- fixture doc-outside
//!     cargo run -p bug2-import-abort -- fixture doc-inside
//!     cargo run -p bug2-import-abort -- minimal doc-outside-forget

use loro::{ExportMode, LoroDoc};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, ExitCode};

const HISTORY: &[u8] = include_bytes!("../fixtures/loro-crash-history.bin");
const UPDATE: &[u8] = include_bytes!("../fixtures/loro-crash-update.bin");

/// Fixture-free: two docs are given the same peer id (the client bug), so
/// op ids 7@0 and 7@1 mean different things in each.
///
/// - `history`: 7@0 = map.insert("k", 0), 7@1 = text.insert(0, "a")
/// - `other`:   7@0..=2 = text.insert(0, "xyz")
///
/// Importing `other` into `history`: 7@0..=1 are "already known", so Loro
/// drops the first two chars of the insert and applies the rest (7@2, "z")
/// at position 0 + 2 = 2 of a 1-char text, which is out of range.
fn minimal() -> (LoroDoc, Vec<u8>) {
    let history = LoroDoc::new();
    history.set_peer_id(7).unwrap();
    history.get_map("m").insert("k", 0).unwrap();
    history.get_text("t").insert(0, "a").unwrap();
    history.commit();

    let other = LoroDoc::new();
    other.set_peer_id(7).unwrap();
    other.get_text("t").insert(0, "xyz").unwrap();
    other.commit();

    (history, other.export(ExportMode::all_updates()).unwrap())
}

/// The reporters' fixtures: a 6-peer document, and an update from peer
/// 2918796465566129979 that rewrites that peer's ops 0..=2 (three map inserts
/// in `history`) as a 7-char text insert plus 10 more ops (0..=16).
fn fixture() -> (LoroDoc, Vec<u8>) {
    let doc = LoroDoc::new();
    doc.import(HISTORY).unwrap();
    (doc, UPDATE.to_vec())
}

fn build(case: &str) -> (LoroDoc, Vec<u8>) {
    match case {
        "minimal" => minimal(),
        "fixture" => fixture(),
        other => panic!("unknown case {other}"),
    }
}

fn run_case(case: &str, mode: &str) {
    match mode {
        // The doc lives outside the closure: catch_unwind does stop the first
        // panic, but the doc is poisoned and its Drop panics later.
        "doc-outside" => {
            let (doc, update) = build(case);
            let caught = catch_unwind(AssertUnwindSafe(|| doc.import(&update))).is_err();
            println!("catch_unwind returned; caught panic = {caught}");
            println!("dropping the doc");
            drop(doc);
            println!("doc dropped cleanly");
        }
        // Only survivable pattern: catch the panic and leak the poisoned doc
        // so its Drop never runs.
        "doc-outside-forget" => {
            let (doc, update) = build(case);
            let caught = catch_unwind(AssertUnwindSafe(|| doc.import(&update))).is_err();
            println!("catch_unwind returned; caught panic = {caught}");
            let still_usable = catch_unwind(AssertUnwindSafe(|| doc.get_deep_value())).is_ok();
            println!("doc usable after the panic = {still_usable}; leaking it with mem::forget");
            std::mem::forget(doc);
        }
        // The doc is owned by the closure, so it is dropped while the first
        // panic unwinds: panic-in-drop during unwinding -> abort.
        "doc-inside" => {
            let caught = catch_unwind(|| {
                let (doc, update) = build(case);
                doc.import(&update).map(|_| ())
            })
            .is_err();
            println!("catch_unwind returned; caught panic = {caught}");
        }
        other => panic!("unknown mode {other}"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [case, mode] = args.as_slice() {
        run_case(case, mode);
        return ExitCode::SUCCESS;
    }
    assert!(args.is_empty(), "usage: bug2-import-abort [<minimal|fixture> <doc-outside|doc-inside>]");

    let exe = std::env::current_exe().unwrap();
    let mut failed = 0;
    let mut total = 0;
    for case in ["minimal", "fixture"] {
        for mode in ["doc-outside", "doc-inside", "doc-outside-forget"] {
            let out = Command::new(&exe).args([case, mode]).output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            println!("=== {case} {mode}: {}", out.status);
            for line in stdout.lines() {
                println!("  stdout: {line}");
            }
            let mut lines = stderr.lines().peekable();
            while let Some(line) = lines.next() {
                if line.contains("panicked at") {
                    let at = line.split("panicked at ").nth(1).unwrap_or(line);
                    // Shorten ~/.cargo/registry/src/index.crates.io-*/<crate>/src/.. to <crate>/src/..
                    let at = match at.find("index.crates.io-") {
                        Some(i) => at[i..].split_once('/').map_or(at, |(_, rest)| rest),
                        None => at,
                    };
                    let msg: String = lines.peek().copied().unwrap_or("").chars().take(90).collect();
                    println!("  panic:  {at} -> {msg}");
                } else if line.contains("non-unwinding panic") {
                    println!("  stderr: {line}");
                }
            }
            if !out.status.success() {
                failed += 1;
            }
            total += 1;
        }
    }
    println!("\n{failed} of {total} runs did not exit cleanly");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
