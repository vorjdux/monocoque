#!/usr/bin/env bash
#
# Drive the monocoque bench peer through a few two-process scenarios and print
# the throughput, cleaning up after itself.
#
# Safety: the bind-side commands (push, push-eager, push-fanout, push-connect and
# their -ipc variants) send in a tight loop until killed, so they would pin a CPU
# core forever if left running. Every spawned PID is tracked and killed on any
# exit (normal, error, or Ctrl-C) via a trap, and every connect-side command runs
# under `timeout`, so the script cannot leave a runaway process or hang the
# machine.
#
# Usage:
#   scripts/run_bench_peer.sh [SIZE] [DURATION_SECS] [WORKERS]
# Examples:
#   scripts/run_bench_peer.sh                 # 64 B, 3 s window, 1 s warmup, 3 workers
#   SIZE=1024 DUR=5 WORKERS=4 scripts/run_bench_peer.sh
#   WARMUP=2 DUR=5 scripts/run_bench_peer.sh  # longer warmup before the timed window

set -uo pipefail

SIZE="${1:-${SIZE:-64}}"
DUR="${2:-${DUR:-3.0}}"
WORKERS="${3:-${WORKERS:-3}}"
# Seconds the connect side receives and discards before the timed window starts,
# so numbers reflect steady state instead of connection ramp-up.
WARMUP="${WARMUP:-1.0}"

PEER="${PEER:-scripts/monocoque_bench_peer/target/release/monocoque_bench_peer}"
# Hard ceiling on how long any single connect-side command may run, so a stuck
# handshake can never hang the script. Generously larger than warmup + duration.
STEP_TIMEOUT="$(awk -v d="$DUR" -v w="$WARMUP" 'BEGIN { printf "%d", d + w + 30 }')"

PIDS=()
TMPDIR="$(mktemp -d)"

cleanup() {
  trap - EXIT INT TERM
  # SIGTERM the loopers first, then SIGKILL anything still alive.
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null; done
  sleep 0.2
  for pid in "${PIDS[@]:-}"; do kill -9 "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$TMPDIR"
}
trap cleanup EXIT INT TERM

if [[ ! -x "$PEER" ]]; then
  echo "Bench peer not built; building release binary..."
  cargo build --release --manifest-path scripts/monocoque_bench_peer/Cargo.toml || exit 1
fi

# start_bind <cmd...> : run a bind-side command in the background and wait for it
# to announce its endpoint ("PORT <n>" for TCP, "PATH <p>" for IPC). Sets
# BIND_PID, BIND_ENDPOINT, BIND_LOG. Returns 1 on failure.
start_bind() {
  BIND_LOG="$(mktemp "$TMPDIR/bind.XXXXXX.log")"
  "$@" >"$BIND_LOG" 2>&1 &
  BIND_PID=$!
  PIDS+=("$BIND_PID")
  BIND_ENDPOINT=""
  local i=0
  while (( i < 100 )); do
    BIND_ENDPOINT="$(sed -n 's/^\(PORT\|PATH\) \(.*\)/\2/p' "$BIND_LOG" | head -1)"
    [[ -n "$BIND_ENDPOINT" ]] && return 0
    if ! kill -0 "$BIND_PID" 2>/dev/null; then
      echo "    bind side exited before announcing an endpoint:"; sed 's/^/      /' "$BIND_LOG"
      return 1
    fi
    sleep 0.1; ((i++))
  done
  echo "    timed out waiting for endpoint"; return 1
}

# Render a "count elapsed size cpu" result line as msg/s and bandwidth.
fmt_result() {
  awk '$1 ~ /^[0-9]+$/ && $2+0 > 0 {
    printf "    %.2f Mmsg/s, %.1f MiB/s  (%s msgs in %.3fs, cpu %.2fs)\n",
      $1/$2/1e6, ($1*$3)/$2/1048576, $1, $2, $4
  }'
}

# bench_pair <label> <bind-subcmd> <connect-subcmd>
#   bind:    $PEER <bind-subcmd> 0 <size>           (announces PORT/PATH)
#   connect: $PEER <connect-subcmd> <endpoint> <size> <dur>
bench_pair() {
  echo "  -- $1 --"
  if ! start_bind "$PEER" "$2" 0 "$SIZE"; then
    return 0
  fi
  timeout "$STEP_TIMEOUT" "$PEER" "$3" "$BIND_ENDPOINT" "$SIZE" "$DUR" "$WARMUP" | fmt_result
  kill "$BIND_PID" 2>/dev/null; wait "$BIND_PID" 2>/dev/null
}

run_throughput_modes() {
  echo "== PUSH/PULL throughput: eager vs coalesced, TCP vs IPC (size=${SIZE}B, dur=${DUR}s) =="
  bench_pair "TCP coalesced" push           pull
  bench_pair "TCP eager"     push-eager     pull
  # IPC is Unix-only; on other platforms the bind side errors and is skipped.
  bench_pair "IPC coalesced" push-ipc       pull-ipc
  bench_pair "IPC eager"     push-ipc-eager pull-ipc
}

run_fanin() {
  echo "== Fan-in  (${WORKERS} PUSH -> 1 sink, size=${SIZE}B, dur=${DUR}s) =="
  # The sink binds, counts for DUR, prints its line, then exits on its own.
  if ! start_bind "$PEER" pull-fanin 0 "$SIZE" "$DUR" "$WORKERS"; then return 0; fi
  local sink_pid="$BIND_PID" sink_log="$BIND_LOG" endpoint="$BIND_ENDPOINT"
  local wpids=()
  for (( w = 0; w < WORKERS; w++ )); do
    "$PEER" push-connect "$endpoint" "$SIZE" >/dev/null 2>&1 &
    wpids+=("$!"); PIDS+=("$!")
  done
  # Wait for the sink to finish, with a hard cap so we never block forever.
  local i=0 maxi=$(( STEP_TIMEOUT * 10 ))
  while kill -0 "$sink_pid" 2>/dev/null; do
    sleep 0.1; (( i++ ))
    if (( i > maxi )); then echo "    WARN: sink overran, killing"; kill "$sink_pid" 2>/dev/null; fi
  done
  fmt_result <"$sink_log"
  for p in "${wpids[@]}"; do kill "$p" 2>/dev/null; wait "$p" 2>/dev/null; done
}

run_fanout() {
  echo "== Fan-out  (1 ventilator -> ${WORKERS} PULL, size=${SIZE}B, dur=${DUR}s) =="
  # The ventilator binds, waits for WORKERS workers, then sends until killed.
  if ! start_bind "$PEER" push-fanout 0 "$SIZE" "$WORKERS"; then return 0; fi
  local vent_pid="$BIND_PID" endpoint="$BIND_ENDPOINT"
  local wpids=() wlogs=()
  for (( w = 0; w < WORKERS; w++ )); do
    local lf; lf="$(mktemp "$TMPDIR/pull.XXXXXX.log")"
    timeout "$STEP_TIMEOUT" "$PEER" pull "$endpoint" "$SIZE" "$DUR" "$WARMUP" >"$lf" 2>&1 &
    wpids+=("$!"); wlogs+=("$lf"); PIDS+=("$!")
  done
  for p in "${wpids[@]}"; do wait "$p" 2>/dev/null; done
  for lf in "${wlogs[@]}"; do fmt_result <"$lf"; done
  kill "$vent_pid" 2>/dev/null; wait "$vent_pid" 2>/dev/null
}

run_throughput_modes
run_fanin
run_fanout
echo "done."
