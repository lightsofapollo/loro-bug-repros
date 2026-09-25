#!/usr/bin/env bash
# Run both repros against several loro versions.
# Usage: scripts/matrix.sh [--release]
# Each version gets a copy of this workspace under matrix/<label>/ with the
# `loro` workspace dependency rewritten; results go to matrix/<label>/*.log.
set -uo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
profile_flag="${1:-}"
MAIN_REV=ad5b2a6d4546473d9f4a96412d2ae6808c8403ec   # loro-dev/loro main on 2026-09-24

# label | workspace dependency line | lockfile pins.
# `loro = "=X"` alone is not enough: loro's own deps on loro-internal,
# loro-common and loro-kv-store are caret requirements, so without the pins
# `loro =1.13.9` silently builds against loro-internal 1.16.2.
specs=(
  '1.13.9|loro = "=1.13.9"|loro-internal@1.13.9 loro-kv-store@1.13.9 loro-common@1.13.1'
  '1.16.0|loro = "=1.16.0"|loro-internal@1.16.0 loro-common@1.16.0 loro-kv-store@1.16.0'
  '1.16.2|loro = "=1.16.2"|loro-internal@1.16.2 loro-common@1.16.0 loro-kv-store@1.16.0'
  "main-ad5b2a6d|loro = { git = \"https://github.com/loro-dev/loro\", rev = \"$MAIN_REV\" }|"
)

printf '%-16s %-10s %-10s\n' version bug1 bug2
for spec in "${specs[@]}"; do
  IFS='|' read -r label dep pins <<<"$spec"
  dir="$root/matrix/$label"
  mkdir -p "$dir"
  cp -Rf Cargo.toml bug1-tree-overflow bug2-import-abort "$dir/"
  rm -f "$dir/Cargo.lock"
  # Replace the `loro = ...` line under [workspace.dependencies].
  awk -v dep="$dep" '/^loro = /{print dep; next} {print}' Cargo.toml >"$dir/Cargo.toml"
  (
    cd "$dir"
    cargo generate-lockfile -q
    for pin in $pins; do
      cargo update -q -p "${pin%@*}" --precise "${pin#*@}" || { echo "pin $pin failed"; exit 1; }
    done
    cargo build -q $profile_flag -p bug1-tree-overflow -p bug2-import-abort >"build.log" 2>&1 || { echo "build failed, see $dir/build.log"; exit 1; }
    resolved=$(cargo tree -q -p bug1-tree-overflow -e normal --prefix none 2>/dev/null \
      | grep -E '^loro(-internal|-common|-kv-store)? v' | sed -E 's/ \(.*//' | sort -u | tr '\n' ' ')
    echo "$resolved" >resolved.txt
    cargo run -q $profile_flag -p bug1-tree-overflow >bug1.log 2>&1; b1=$?
    cargo run -q $profile_flag -p bug2-import-abort >bug2.log 2>&1; b2=$?
    r1=$([ $b1 -eq 0 ] && echo ok || echo CRASH)
    r2=$([ $b2 -eq 0 ] && echo ok || echo CRASH)
    printf '%-16s %-10s %-10s (%s)\n' "$label" "$r1" "$r2" "$resolved"
  )
done
echo "logs: $root/matrix/<version>/bug{1,2}.log"
