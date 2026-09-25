#!/usr/bin/env bash
# Print the collapsed recursion stack for one bug-1 probe (macOS/lldb).
# Usage: scripts/backtrace.sh [probe]   (default: fork_at)
# A 256 KiB stack keeps the recursion short enough for lldb to print it all.
set -euo pipefail
cd "$(dirname "$0")/.."
probe="${1:-fork_at}"
cargo build -q -p bug1-tree-overflow
out="$(mktemp)"
trap 'rm -f "$out"' EXIT
(ulimit -s 256; lldb --batch -o run -k "thread backtrace -c 100000" -k quit -- \
  target/debug/bug1-tree-overflow "$probe" >"$out" 2>&1) || true
grep "frame #" "$out" \
  | sed -E 's/^ *\*? *frame #[0-9]+: 0x[0-9a-f]+ [^`]*`//; s/\(.*\) at / @ /; s/ \[inlined\]//' \
  | grep -E 'loro|bug1' | grep -v 'hash::map' | cut -c1-160 | uniq -c
