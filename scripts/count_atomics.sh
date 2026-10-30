#!/usr/bin/env bash
# Count atomic instructions per message under callgrind, per backend.
#
#   scripts/count_atomics.sh [compio|tokio|smol]
#
# Prints, for that backend:
#   roundtrip   atomics per PUSH -> PULL message over TCP (send, receive,
#               the reader and writer tasks, and the runtime)
#   queued      atomics per borrowed receive of a message already queued,
#               which is meant to be 0
#
# Needs valgrind and objdump. The probe is built non-PIE with symbols so the
# addresses callgrind records match the binary's own; --target keeps those
# flags off build scripts and proc macros.
set -euo pipefail
backend="${1:-compio}"
case "$backend" in
  compio) features="--features zmq" ;;
  tokio) features="--no-default-features --features runtime-tokio,zmq" ;;
  smol) features="--no-default-features --features runtime-smol,zmq" ;;
  *) echo "unknown backend: $backend" >&2; exit 2 ;;
esac
root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$root/target/atomics-$backend"
triple="$(rustc -vV | sed -n 's/^host: //p')"
# shellcheck disable=SC2086
CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1 \
  RUSTFLAGS="-C relocation-model=static" \
  cargo build -q --release --target "$triple" --example atomics_probe $features \
  --target-dir "$dir" --manifest-path "$root/Cargo.toml"
bin="$dir/$triple/release/examples/atomics_probe"
out="$(mktemp -d)"
trap 'rm -r "$out"' EXIT
# Spinning before sleeping adds polling loops that are not per-message work.
export MONOCOQUE_SPIN_US=0
cg() { valgrind --tool=callgrind --dump-instr=yes --dump-line=no --callgrind-out-file="$1" "${@:2}" >/dev/null 2>&1; }

cg "$out/rt-a" "$bin" roundtrip 2000
cg "$out/rt-b" "$bin" roundtrip 4000
echo "== $backend roundtrip (per message)"
python3 "$root/scripts/count_atomics.py" "$bin" "$out/rt-b" "$out/rt-a" 2000

cg "$out/q-a" "$bin" queued 5000 0
cg "$out/q-b" "$bin" queued 5000 5000
echo "== $backend queued borrowed receive (per message)"
python3 "$root/scripts/count_atomics.py" "$bin" "$out/q-b" "$out/q-a" 5000
