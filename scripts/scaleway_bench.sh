#!/usr/bin/env bash
#
# Rent one Scaleway Elastic Metal server, run monocoque's benchmarks on it,
# copy the results back and delete the server.
#
# The server is billed by the hour from the moment it is created until it is
# DELETED. Powering it off does not stop the billing. Everything here is built
# around that: the server is deleted on every way out of this script, and
# `--reap` deletes any that a crashed or killed run left behind.
#
# Usage:
#   scripts/scaleway_bench.sh [--mode suite|pair|instr|arena] [options]
#   scripts/scaleway_bench.sh --reap [--max-age 4h] [--server-id ID]
#   scripts/scaleway_bench.sh --help
#
# Credentials come from the environment only: SCW_ACCESS_KEY, SCW_SECRET_KEY,
# SCW_DEFAULT_PROJECT_ID and SCW_DEFAULT_ORGANIZATION_ID.

# The cleanup and everything it calls run only from a trap, which shellcheck
# reads as unreachable code.
# shellcheck disable=SC2317

set -euo pipefail

# ── Defaults, each one overridable by a flag or by the variable named ──────

ZONE="${SCW_BENCH_ZONE:-fr-par-1}"
OFFER="${SCW_BENCH_OFFER:-EM-B130E-NVME}"
OS_NAME="${SCW_BENCH_OS_NAME:-Ubuntu}"
OS_VERSION="${SCW_BENCH_OS_VERSION:-26.04}"
TAG="${SCW_BENCH_TAG:-monocoque-bench}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
SSH_KEY_ID="${SCW_BENCH_SSH_KEY_ID:-}"
SSH_USER="${SCW_BENCH_SSH_USER:-}"
HOURLY_PRICE="${SCW_BENCH_HOURLY_PRICE:-}"
FALLBACK_HOURLY_PRICE="0.247"
API_URL="${SCW_API_URL:-https://api.scaleway.com}"

MODE="suite"
REF="HEAD"
AGAINST=""
BACKEND="all"
ROUNDS=5
PIN=""
ARENA_CMD=""
BENCHES=()
MODE_TIMEOUT_MIN=150
HARD_CAP_MIN=240
READY_TIMEOUT_MIN=75
DELETE_TIMEOUT_MIN=15
DRY_RUN=false
KEEP=false
FORCE=false
REAP=false
REAP_MAX_AGE="4h"
REAP_SERVER_ID=""
MAX_LIFETIME_MIN="${SCW_BENCH_MAX_LIFETIME_MIN:-}"
WATCHDOG=false
WATCHDOG_DEADLINE=""
WATCHDOG_NAME=""
WATCHDOG_PARENT=""
WATCHDOG_PID=""
WATCHDOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/monocoque-bench"

STATE_FILE=".scaleway-bench-server"
RESULTS_ROOT="bench-results"

# ── State the cleanup needs ────────────────────────────────────────────────

SERVER_ID=""
SERVER_NAME=""
SERVER_IP=""
CREATE_REQUESTED=false
CREATE_EPOCH=0
READY_EPOCH=0
START_EPOCH=$(date -u +%s)
WORKDIR=""
RESULTS_DIR=""
REF_SHA=""
AGAINST_SHA=""
OFFER_ID=""
OS_ID=""
CHILD_PID=""
BUILD_EXIT=""
MODE_EXIT=""
DELETED="not created"
RESULTS_COLLECTED=false

if [[ -t 2 ]]; then
    RED=$'\033[0;31m' YELLOW=$'\033[1;33m' GREEN=$'\033[0;32m' NC=$'\033[0m'
else
    RED="" YELLOW="" GREEN="" NC=""
fi

# Messages go to a copy of the terminal's stderr. A signal handler runs with
# whatever redirection the interrupted command had, and the cleanup's report
# of what happened to the server must not end up in that command's log file.
exec 3>&2

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&3; }
warn() { printf '%s%s %s%s\n' "$YELLOW" "$(date -u +%H:%M:%S)" "$*" "$NC" >&3; }
red() { printf '%s%s%s\n' "$RED" "$*" "$NC" >&3; }
die() {
    red "error: $*"
    exit 1
}

usage() {
    cat << EOF
Rent one Scaleway Elastic Metal server, benchmark monocoque on it, copy the
results back and delete the server. The server bills by the hour until it is
deleted; powering it off does not stop the billing.

Usage:
  $0 [options]              run a benchmark
  $0 --reap [options]       delete leftover servers

Modes:
  --mode suite              scripts/bench_all.sh on --ref (default)
  --mode pair               scripts/bench_pair.py, --ref against --against
  --mode instr              instr_hotpath and scripts/check_instr_baseline.py
  --mode arena              run --arena-cmd in the unpacked source tree

What to measure:
  --ref REF                 revision under test (default HEAD). The committed
                            tree is shipped; uncommitted changes are not
  --against REF             the other revision, required by --mode pair
  --backend NAME            compio, tokio, smol or all (default all)
  --bench NAME              only this benchmark; may be repeated
  --rounds N                rounds per revision in pair mode (default 5)
  --pin CORES               cores for the two-thread benchmarks (default 2,4,
                            checked on the server and replaced if they are not
                            two separate physical cores)
  --arena-cmd CMD           command for --mode arena

Where to run:
  --zone ZONE               default $ZONE [SCW_BENCH_ZONE]
  --offer NAME              default $OFFER, hourly [SCW_BENCH_OFFER]
  --os-name NAME            default $OS_NAME [SCW_BENCH_OS_NAME]
  --os-version PREFIX       default $OS_VERSION [SCW_BENCH_OS_VERSION]
  --tag TAG                 default $TAG [SCW_BENCH_TAG]
  --ssh-key PATH            private key (default ~/.ssh/id_ed25519) [SSH_KEY].
                            Its public key must be registered in the project
  --ssh-key-id ID           skip the lookup of that key [SCW_BENCH_SSH_KEY_ID]
  --ssh-user USER           login user (default: the OS image's own)
                            [SCW_BENCH_SSH_USER]
  --hourly-price EUR        for the cost estimate (default: the offer's price)
                            [SCW_BENCH_HOURLY_PRICE]

Limits:
  --mode-timeout MIN        the benchmark itself (default $MODE_TIMEOUT_MIN)
  --hard-cap MIN            the whole script (default $HARD_CAP_MIN)
  --max-lifetime MIN        the server itself, counted from its creation
                            (default: the hard cap plus 15). A detached
                            watchdog deletes the server at that age whatever
                            state this script is in, and sooner if this
                            script dies without deleting it. Also applies to
                            --keep [SCW_BENCH_MAX_LIFETIME_MIN]
  --ready-timeout MIN       delivery and installation (default $READY_TIMEOUT_MIN)

Safety:
  --dry-run                 print every command, create nothing, need no
                            credentials
  --keep                    do not delete the server at the end
  --force                   start even if a server tagged $TAG exists
  --reap                    delete every server tagged $TAG older than
                            --max-age, in every zone; non-zero if one remains
  --max-age AGE             for --reap: 4h, 30m, 0 (default $REAP_MAX_AGE)
  --server-id ID            for --reap: only this server (still must be tagged)
  --help, -h                this text

Environment: SCW_ACCESS_KEY, SCW_SECRET_KEY, SCW_DEFAULT_PROJECT_ID,
SCW_DEFAULT_ORGANIZATION_ID.

Results land in $RESULTS_ROOT/<UTC date>-<short sha>-<mode>/.
EOF
}

# ── Arguments ──────────────────────────────────────────────────────────────

need_value() {
    [[ $# -ge 2 ]] || die "$1 needs a value"
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --mode) need_value "$@"; MODE="$2"; shift 2 ;;
        --ref) need_value "$@"; REF="$2"; shift 2 ;;
        --against) need_value "$@"; AGAINST="$2"; shift 2 ;;
        --backend) need_value "$@"; BACKEND="$2"; shift 2 ;;
        --bench) need_value "$@"; BENCHES+=("$2"); shift 2 ;;
        --rounds) need_value "$@"; ROUNDS="$2"; shift 2 ;;
        --pin) need_value "$@"; PIN="$2"; shift 2 ;;
        --arena-cmd) need_value "$@"; ARENA_CMD="$2"; shift 2 ;;
        --zone) need_value "$@"; ZONE="$2"; shift 2 ;;
        --offer) need_value "$@"; OFFER="$2"; shift 2 ;;
        --os-name) need_value "$@"; OS_NAME="$2"; shift 2 ;;
        --os-version) need_value "$@"; OS_VERSION="$2"; shift 2 ;;
        --tag) need_value "$@"; TAG="$2"; shift 2 ;;
        --ssh-key) need_value "$@"; SSH_KEY="$2"; shift 2 ;;
        --ssh-key-id) need_value "$@"; SSH_KEY_ID="$2"; shift 2 ;;
        --ssh-user) need_value "$@"; SSH_USER="$2"; shift 2 ;;
        --hourly-price) need_value "$@"; HOURLY_PRICE="$2"; shift 2 ;;
        --mode-timeout) need_value "$@"; MODE_TIMEOUT_MIN="$2"; shift 2 ;;
        --hard-cap) need_value "$@"; HARD_CAP_MIN="$2"; shift 2 ;;
        --ready-timeout) need_value "$@"; READY_TIMEOUT_MIN="$2"; shift 2 ;;
        --max-age) need_value "$@"; REAP_MAX_AGE="$2"; shift 2 ;;
        --server-id) need_value "$@"; REAP_SERVER_ID="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        --keep) KEEP=true; shift ;;
        --force) FORCE=true; shift ;;
        --reap) REAP=true; shift ;;
        --max-lifetime) need_value "$@"; MAX_LIFETIME_MIN="$2"; shift 2 ;;
        --watchdog)
            # Internal: how this script starts its own detached watchdog.
            [[ $# -ge 5 ]] || die "--watchdog is internal"
            WATCHDOG=true
            WATCHDOG_DEADLINE="$2" WATCHDOG_NAME="$3" ZONE="$4" WATCHDOG_PARENT="$5"
            shift 5
            ;;
        --help | -h) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

case "$MODE" in
    suite | pair | instr | arena) ;;
    *) die "unknown mode: $MODE (suite, pair, instr or arena)" ;;
esac
case "$BACKEND" in
    compio | tokio | smol | all) ;;
    *) die "unknown backend: $BACKEND (compio, tokio, smol or all)" ;;
esac
[[ -n "$MAX_LIFETIME_MIN" ]] || MAX_LIFETIME_MIN=$((HARD_CAP_MIN + 15))
for n in "$ROUNDS" "$MODE_TIMEOUT_MIN" "$HARD_CAP_MIN" "$READY_TIMEOUT_MIN" "$MAX_LIFETIME_MIN"; do
    [[ "$n" =~ ^[0-9]+$ ]] || die "not a whole number: $n"
done
if [[ -n "$PIN" ]] && ! [[ "$PIN" =~ ^[0-9]+,[0-9]+$ ]]; then
    die "--pin takes two CPU numbers, for example 2,4"
fi
if ! $REAP; then
    if [[ "$MODE" == pair && -z "$AGAINST" ]]; then
        die "--mode pair needs --against REF"
    fi
    if [[ "$MODE" == arena && -z "$ARENA_CMD" ]]; then
        die "--mode arena needs --arena-cmd CMD"
    fi
fi

# ── Running commands ───────────────────────────────────────────────────────

# Print a command the way it could be pasted into a shell.
show() {
    local out="+" arg
    for arg in "$@"; do
        if [[ "$arg" =~ ^[A-Za-z0-9_./:=@,+-]+$ ]]; then
            out+=" $arg"
        else
            out+=" ${arg@Q}"
        fi
    done
    printf '%s\n' "$out" >&3
}

# A read-only scw call. In a dry run it is printed and the placeholder given
# as the first argument is returned instead of its output.
scw_read() {
    local placeholder="$1"
    shift
    if $DRY_RUN; then
        show scw "$@" -o json
        printf '%s\n' "$placeholder"
        return 0
    fi
    scw "$@" -o json
}

# Bash runs a trap only after the foreground command returns, so a command
# that may run for minutes goes to the background and is waited for: a signal
# then interrupts the wait at once, and the handler stops the child.
interruptible() {
    "$@" &
    CHILD_PID=$!
    local rc=0
    wait "$CHILD_PID" || rc=$?
    CHILD_PID=""
    return "$rc"
}

pause() {
    interruptible sleep "$1"
}

check_hard_cap() {
    local now
    now=$(date -u +%s)
    if ((now - START_EPOCH > HARD_CAP_MIN * 60)); then
        die "hard cap of ${HARD_CAP_MIN} minutes reached"
    fi
}

require_tools() {
    local tool
    for tool in "$@"; do
        command -v "$tool" > /dev/null 2>&1 || die "$tool is not installed"
    done
}

require_credentials() {
    local name
    for name in SCW_ACCESS_KEY SCW_SECRET_KEY SCW_DEFAULT_PROJECT_ID SCW_DEFAULT_ORGANIZATION_ID; do
        [[ -n "${!name:-}" ]] || die "$name is not set"
    done
}

# ── Deleting ───────────────────────────────────────────────────────────────

# Is the server still listed? 0 yes, 1 no, 2 could not tell.
server_listed() {
    local id="$1" zone="$2" listing
    listing=$(scw baremetal server list "zone=$zone" -o json 2> /dev/null) || return 2
    jq -e --arg id "$id" 'any(.[]; .id == $id)' <<< "$listing" > /dev/null 2>&1 && return 0
    jq -e 'type == "array"' <<< "$listing" > /dev/null 2>&1 || return 2
    return 1
}

# Delete a server and keep at it until a listing no longer shows it. Asking
# is not enough: a delete can be refused while the server is in a transient
# state, and only its absence from the listing proves the billing stopped.
delete_until_gone() {
    local id="$1" zone="$2"
    local deadline=$(($(date -u +%s) + DELETE_TIMEOUT_MIN * 60)) delay=10 listed
    if $DRY_RUN; then
        show scw baremetal server delete "$id" "zone=$zone"
        show scw baremetal server list "zone=$zone" -o json
        return 0
    fi
    while true; do
        show scw baremetal server delete "$id" "zone=$zone"
        scw baremetal server delete "$id" "zone=$zone" > /dev/null 2>&1 || true
        listed=0
        server_listed "$id" "$zone" || listed=$?
        if ((listed == 1)); then
            log "server $id is gone"
            return 0
        fi
        if (($(date -u +%s) >= deadline)); then
            return 1
        fi
        log "server $id is still listed, trying again in ${delay}s"
        sleep "$delay"
        delay=$((delay < 60 ? delay * 2 : 60))
    done
}

print_still_exists() {
    local id="$1" zone="$2"
    red "════════════════════════════════════════════════════════════════════"
    red "SERVER $id IN $zone STILL EXISTS AND IS STILL BEING BILLED"
    red "Delete it by hand, then check that it is gone:"
    red "  scw baremetal server delete $id zone=$zone"
    red "  scw baremetal server list zone=$zone"
    red "════════════════════════════════════════════════════════════════════"
}

# "4h", "30m", "90s" or a bare number of seconds.
age_seconds() {
    case "$1" in
        *h) echo $((${1%h} * 3600)) ;;
        *m) echo $((${1%m} * 60)) ;;
        *s) echo "${1%s}" ;;
        *) echo "$1" ;;
    esac
}

reap() {
    require_tools scw jq
    require_credentials
    [[ "$REAP_MAX_AGE" =~ ^[0-9]+[hms]?$ ]] || die "--max-age takes 4h, 30m, 90s or 0"
    local max_age listing now victims remaining=0 id zone name age
    max_age=$(age_seconds "$REAP_MAX_AGE")
    now=$(date -u +%s)

    show scw baremetal server list zone=all "tags.0=$TAG" -o json
    listing=$(scw baremetal server list zone=all "tags.0=$TAG" -o json) \
        || die "could not list servers; nothing was checked"

    # The tag is checked again here: nothing without it is ever deleted,
    # whatever the listing returned and whatever --server-id names.
    victims=$(jq -r --arg tag "$TAG" --arg only "$REAP_SERVER_ID" --argjson now "$now" \
        --argjson max "$max_age" '
        .[]
        | select((.tags // []) | index($tag))
        | select($only == "" or .id == $only)
        | (.created_at // "" | sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z")) as $created
        | (try ($created | fromdateiso8601) catch 0) as $born
        | select($now - $born >= $max)
        | [.id, .zone, .name, ($now - $born | tostring)] | @tsv' <<< "$listing")

    if [[ -z "$victims" ]]; then
        log "no server tagged $TAG older than $REAP_MAX_AGE"
        return 0
    fi

    while IFS=$'\t' read -r id zone name age; do
        warn "reaping $name ($id) in $zone, $((age / 60)) minutes old"
        if delete_until_gone "$id" "$zone"; then
            if [[ -f "$STATE_FILE" ]] && grep -q "$id" "$STATE_FILE"; then
                rm -f "$STATE_FILE"
            fi
        else
            print_still_exists "$id" "$zone"
            remaining=$((remaining + 1))
        fi
    done <<< "$victims"

    if ((remaining > 0)); then
        red "$remaining server(s) could not be deleted"
        return 1
    fi
    printf '%sall leftover servers are gone%s\n' "$GREEN" "$NC" >&3
}

# The watchdog is this script again, started detached just before the server
# is created. It does not depend on the run that started it: it survives that
# run being killed outright (kill -9, a closed terminal, a hung SSH session
# that never returns) and deletes the server by its unique name
#   - as soon as the run is gone without having deleted it, or
#   - when the server reaches its maximum lifetime, even if the run is still
#     alive and stuck, and even if it was started with --keep.
# What it cannot survive is this machine going down. The hourly reaper in CI
# and the billing alert are the nets for that.
watchdog() {
    require_tools scw jq
    require_credentials
    local keep_marker="$WATCHDOG_DIR/$WATCHDOG_NAME.keep" listing id why=""
    log "watchdog for $WATCHDOG_NAME in $ZONE, deadline $(date -u -d "@$WATCHDOG_DEADLINE" +%FT%TZ)"
    while true; do
        if (($(date -u +%s) >= WATCHDOG_DEADLINE)); then
            why="the server reached its maximum lifetime"
            break
        fi
        if ! kill -0 "$WATCHDOG_PARENT" 2> /dev/null && [[ ! -e "$keep_marker" ]]; then
            # Give a run that is just finishing its own deletion a moment.
            sleep 20
            why="the run that created it is gone"
            break
        fi
        sleep 30
    done
    # Keep at it for six hours: a server that will not go away is exactly
    # the case this exists for.
    local give_up=$(($(date -u +%s) + 6 * 3600))
    while (($(date -u +%s) < give_up)); do
        if listing=$(scw baremetal server list "zone=$ZONE" "name=$WATCHDOG_NAME" -o json 2> /dev/null) \
            && jq -e 'type == "array"' <<< "$listing" > /dev/null 2>&1; then
            id=$(jq -r --arg name "$WATCHDOG_NAME" --arg tag "$TAG" \
                '[.[] | select(.name == $name) | select((.tags // []) | index($tag))][0].id // empty' <<< "$listing")
            if [[ -z "$id" ]]; then
                log "no server named $WATCHDOG_NAME is listed; nothing left to do"
                rm -f "$keep_marker"
                return 0
            fi
            warn "deleting $id: $why"
            if delete_until_gone "$id" "$ZONE"; then
                rm -f "$keep_marker"
                return 0
            fi
            print_still_exists "$id" "$ZONE"
        else
            warn "could not list servers; trying again"
        fi
        sleep 120
    done
    red "watchdog gave up after six hours; $WATCHDOG_NAME may still exist"
    return 1
}

if $WATCHDOG; then
    watchdog
    exit $?
fi

if $REAP; then
    reap
    exit $?
fi

# ── SSH ────────────────────────────────────────────────────────────────────

ssh_options() {
    printf '%s\n' -i "$SSH_KEY" \
        -o IdentitiesOnly=yes -o BatchMode=yes \
        -o StrictHostKeyChecking=accept-new \
        -o "UserKnownHostsFile=$WORKDIR/known_hosts" \
        -o ConnectTimeout=15 -o ServerAliveInterval=30 -o ServerAliveCountMax=6 \
        -o LogLevel=ERROR
}

# remote <seconds> <command>: run a command on the server, bounded in time.
remote() {
    local limit="$1"
    shift
    local opts
    mapfile -t opts < <(ssh_options)
    if $DRY_RUN; then
        show ssh "$SSH_USER@$SERVER_IP" "$@"
        return 0
    fi
    interruptible timeout "$limit" ssh "${opts[@]}" "$SSH_USER@$SERVER_IP" "$@"
}

upload() {
    local opts
    mapfile -t opts < <(ssh_options)
    if $DRY_RUN; then
        show scp "$1" "$SSH_USER@$SERVER_IP:$2"
        return 0
    fi
    interruptible timeout 600 scp -q "${opts[@]}" "$1" "$SSH_USER@$SERVER_IP:$2"
}

download() {
    local opts
    mapfile -t opts < <(ssh_options)
    timeout 900 scp -q "${opts[@]}" "$SSH_USER@$SERVER_IP:$1" "$2"
}

# ── What runs on the server ────────────────────────────────────────────────

# Turns SMT off and sets the performance governor, then records what the
# machine looked like. Used by cloud-init, and again over SSH if cloud-init
# did not run it.
bench_tune_script() {
    cat << 'EOF'
#!/bin/sh
echo off > /sys/devices/system/cpu/smt/control 2>/dev/null || true
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done 2>/dev/null || true
{ date -u; uname -r; grep -m1 microcode /proc/cpuinfo; lscpu;
  echo "smt: $(cat /sys/devices/system/cpu/smt/control)";
  echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)";
  echo "boost: $(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null)"; } > /root/bench-host.txt
EOF
}

SERVER_PACKAGES="build-essential cmake pkg-config git valgrind python3 util-linux libzmq3-dev linux-tools-common linux-tools-generic"

# No secret goes in here: user data is readable by anything on the server.
cloud_config() {
    local user="$1" as_user
    if [[ "$user" == root ]]; then
        as_user="sh -c"
    else
        as_user="su - $user -c"
    fi
    cat << EOF
#cloud-config
package_update: true
packages: [${SERVER_PACKAGES// /, }]
write_files:
  - path: /usr/local/bin/bench-tune
    permissions: '0755'
    content: |
$(bench_tune_script | sed 's/^/      /')
runcmd:
  - /usr/local/bin/bench-tune
  - $as_user 'curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal'
EOF
}

# The same preparation as the cloud-config, for an image that ignored it.
prepare_script() {
    cat << EOF
#!/bin/bash
set -euo pipefail
SUDO=""
[ "\$(id -u)" = 0 ] || SUDO="sudo"
if ! \$SUDO test -f /root/bench-host.txt; then
    \$SUDO env DEBIAN_FRONTEND=noninteractive apt-get update -q
    \$SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -q $SERVER_PACKAGES
    \$SUDO install -m 0755 "\$HOME/bench/bench-tune" /usr/local/bin/bench-tune
    \$SUDO /usr/local/bin/bench-tune
fi
if ! [ -x "\$HOME/.cargo/bin/cargo" ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
EOF
}

# Unpacks the source, builds, picks the pin cores and runs the mode. Its
# settings arrive in run.env next to it. Each stage leaves its exit code in a
# file, so the outcome survives a dropped SSH connection.
run_script() {
    cat << 'EOF'
#!/bin/bash
set -uo pipefail
cd "$HOME/bench" || exit 1
# shellcheck disable=SC1091
. ./run.env
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
SUDO=""
[ "$(id -u)" = 0 ] || SUDO="sudo"
mkdir -p logs
exec > logs/run.log 2>&1

stage() { echo "$1" > "$HOME/bench/logs/stage"; echo "== $(date -u +%H:%M:%S) $1"; }

stage unpack
rm -rf repo && mkdir repo && cd repo || exit 1
if [ -n "$AGAINST_TAG" ]; then
    # bench_pair.py checks the other revision out of git, so both trees go
    # into a small local repository: no clone, and no credentials here.
    git init -q -b bench-run
    git config user.email bench@localhost
    git config user.name bench
    tar xzf ../against.tar.gz
    git add -A -f && git commit -q -m "against" && git tag "$AGAINST_TAG"
    find . -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
    tar xzf ../src.tar.gz
    git add -A -f && git commit -q -m "ref"
else
    tar xzf ../src.tar.gz
fi

stage host
{
    echo "kernel: $(uname -r)"
    echo "microcode: $(grep -m1 microcode /proc/cpuinfo | cut -d: -f2 | xargs)"
    echo "smt: $(cat /sys/devices/system/cpu/smt/control 2>/dev/null)"
    echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)"
    echo "boost: $(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null)"
    echo "rustc: $(rustc --version 2>/dev/null | tail -1)"
} > ../logs/host.txt
lscpu > ../logs/lscpu.txt
lscpu -e > ../logs/lscpu-e.txt
cloud-init status --long > ../logs/cloud-init.txt 2>&1
$SUDO cat /root/bench-host.txt > ../logs/bench-host.txt 2>/dev/null

# Two online CPUs on two different physical cores, neither of them core 0,
# which takes most of the interrupts. The requested pair is kept if it
# passes; otherwise the first two cores that do are used.
stage pin
cores=$(lscpu -p=CPU,CORE,ONLINE | grep -v '^#')
core_of() { echo "$cores" | awk -F, -v c="$1" '$1 == c && $3 == "Y" {print $2}'; }
a=${PIN%,*} b=${PIN#*,}
ca=$(core_of "$a") cb=$(core_of "$b")
if [ -z "$ca" ] || [ -z "$cb" ] || [ "$ca" = "$cb" ] || [ "$ca" = 0 ] || [ "$cb" = 0 ]; then
    picked=$(echo "$cores" | awk -F, '$3 == "Y" && $2 != 0 && !seen[$2]++ {print $1}' | head -2 | paste -sd,)
    echo "pin $PIN is not two separate physical cores here; using $picked"
    PIN=$picked
fi
case "$PIN" in
    *,*) ;;
    *) echo "could not find two physical cores to pin to"; echo 1 > ../logs/build.exit; exit 1 ;;
esac
echo "$PIN" > ../logs/pin.txt
echo "pinned to $PIN"

stage build
timeout 45m cargo bench -p monocoque-rs --features zmq --no-run > ../logs/build.log 2>&1
rc=$?
echo "$rc" > ../logs/build.exit
[ "$rc" -eq 0 ] || exit "$rc"

stage "$MODE"
uptime > ../logs/uptime-before.txt
bench_flags=()
for b in $BENCHES; do bench_flags+=(--bench "$b"); done
case "$MODE" in
    suite)
        timeout "${MODE_TIMEOUT_MIN}m" ./scripts/bench_all.sh --backend "$BACKEND" --pin "$PIN" \
            "${bench_flags[@]}" > ../logs/mode.log 2>&1
        ;;
    pair)
        timeout "${MODE_TIMEOUT_MIN}m" ./scripts/bench_pair.py --against "$AGAINST_TAG" \
            --backend "$BACKEND" --pin "$PIN" --rounds "$ROUNDS" \
            "${bench_flags[@]}" > ../logs/mode.log 2>&1
        ;;
    instr)
        # The runner has to match the gungraun library version, so it is read
        # from the lock file of the tree under test.
        ver=$(grep -A1 '^name = "gungraun"$' Cargo.lock | grep '^version = ' | head -1 | sed -E 's/^version = "(.*)"/\1/')
        {
            # shellcheck disable=SC2016
            timeout "${MODE_TIMEOUT_MIN}m" sh -c '
                set -e
                cargo install gungraun-runner --version "$1" --locked
                cargo bench -p monocoque-rs --bench instr_hotpath --features zmq > ../logs/instr.out 2>&1
                python3 scripts/check_instr_baseline.py ../logs/instr.out' sh "$ver"
        } > ../logs/mode.log 2>&1
        ;;
    arena)
        timeout "${MODE_TIMEOUT_MIN}m" bash -c "$ARENA_CMD" > ../logs/mode.log 2>&1
        ;;
esac
rc=$?
echo "$rc" > ../logs/mode.exit
uptime > ../logs/uptime-after.txt
stage finished
exit "$rc"
EOF
}

# Packs whatever results exist, including from a run that was cut short.
collect_script() {
    cat << 'EOF'
set -u
cd "$HOME/bench" || exit 1
uptime > logs/uptime-at-collect.txt 2>/dev/null
paths="logs"
for p in repo/target/criterion repo/bench_results \
    repo/target/bench-pair/run-old/criterion repo/target/bench-pair/run-new/criterion; do
    [ -e "$p" ] && paths="$paths $p"
done
# shellcheck disable=SC2086
tar czf results.tgz $paths
EOF
}

# ── Results and cleanup ────────────────────────────────────────────────────

read_exit_code() {
    [[ -f "$RESULTS_DIR/logs/$1.exit" ]] && tr -d '[:space:]' < "$RESULTS_DIR/logs/$1.exit"
    return 0
}

collect_results() {
    $DRY_RUN && {
        show ssh "$SSH_USER@$SERVER_IP" "bash -s < collect.sh"
        show scp "$SSH_USER@$SERVER_IP:bench/results.tgz" "$RESULTS_DIR/"
        return 0
    }
    [[ -n "$SERVER_IP" && -n "$SSH_USER" ]] || return 0
    local opts
    mapfile -t opts < <(ssh_options)
    log "copying the results back"
    mkdir -p "$RESULTS_DIR"
    if collect_script | timeout 300 ssh "${opts[@]}" "$SSH_USER@$SERVER_IP" 'bash -s' \
        && download "bench/results.tgz" "$WORKDIR/results.tgz" \
        && tar xzf "$WORKDIR/results.tgz" -C "$RESULTS_DIR"; then
        RESULTS_COLLECTED=true
        if [[ -d "$RESULTS_DIR/repo/target" ]]; then
            mv "$RESULTS_DIR/repo/target" "$RESULTS_DIR/target"
        fi
        if [[ -d "$RESULTS_DIR/repo/bench_results" ]]; then
            mv "$RESULTS_DIR/repo/bench_results" "$RESULTS_DIR/bench_results"
        fi
        rmdir "$RESULTS_DIR/repo" 2> /dev/null || true
    else
        warn "could not copy the results (the server may not have got that far)"
    fi
}

write_run_json() {
    local script_exit="$1" end_epoch
    end_epoch=$(date -u +%s)
    mkdir -p "$RESULTS_DIR"
    jq -n \
        --arg zone "$ZONE" --arg offer "$OFFER" --arg offer_id "$OFFER_ID" \
        --arg os_id "$OS_ID" --arg server_id "$SERVER_ID" --arg server_name "$SERVER_NAME" \
        --arg ref "$REF" --arg ref_sha "$REF_SHA" \
        --arg against "$AGAINST" --arg against_sha "$AGAINST_SHA" \
        --arg mode "$MODE" --arg backend "$BACKEND" --arg rounds "$ROUNDS" \
        --arg benches "${BENCHES[*]:-}" \
        --arg pin "$(cat "$RESULTS_DIR/logs/pin.txt" 2> /dev/null || echo "$PIN")" \
        --arg started "$(date -u -d "@$START_EPOCH" +%FT%TZ)" \
        --arg created "$( ((CREATE_EPOCH > 0)) && date -u -d "@$CREATE_EPOCH" +%FT%TZ)" \
        --arg ready "$( ((READY_EPOCH > 0)) && date -u -d "@$READY_EPOCH" +%FT%TZ)" \
        --arg ended "$(date -u -d "@$end_epoch" +%FT%TZ)" \
        --arg build_exit "${BUILD_EXIT:-$(read_exit_code build)}" \
        --arg mode_exit "${MODE_EXIT:-$(read_exit_code mode)}" \
        --arg script_exit "$script_exit" --arg deleted "$DELETED" \
        --arg price "${HOURLY_PRICE:-$FALLBACK_HOURLY_PRICE}" \
        --argjson provisioning_seconds "$( ((READY_EPOCH > 0)) && echo $((READY_EPOCH - CREATE_EPOCH)) || echo null)" \
        --argjson billed_seconds "$( ((CREATE_EPOCH > 0)) && echo $((end_epoch - CREATE_EPOCH)) || echo null)" \
        --argjson results_collected "$RESULTS_COLLECTED" \
        '{zone: $zone, offer: $offer, offer_id: $offer_id, os_id: $os_id,
          server_id: $server_id, server_name: $server_name,
          ref: $ref, ref_sha: $ref_sha, against: $against, against_sha: $against_sha,
          mode: $mode, backend: $backend, rounds: $rounds, benches: $benches, pin: $pin,
          started_at: $started, created_at: $created, ready_at: $ready, ended_at: $ended,
          provisioning_seconds: $provisioning_seconds, billed_seconds: $billed_seconds,
          hourly_price_eur: $price,
          exit_codes: {build: $build_exit, mode: $mode_exit, script: $script_exit},
          results_collected: $results_collected, server_deleted: $deleted}' \
        > "$RESULTS_DIR/run.json"
}

print_cost() {
    ((CREATE_EPOCH > 0)) || return 0
    local seconds price
    seconds=$(($(date -u +%s) - CREATE_EPOCH))
    price="${HOURLY_PRICE:-$FALLBACK_HOURLY_PRICE}"
    awk -v s="$seconds" -v p="$price" 'BEGIN {
        h = s / 3600; whole = int(h); if (whole < h) whole++
        printf "billed time: %dh %02dm (%d s)\n", s / 3600, (s % 3600) / 60, s
        printf "estimated cost: EUR %.2f at EUR %s/h, or EUR %.2f if every started hour is charged\n",
            h * p, p, whole * p
    }' >&3
}

cleanup() {
    local rc=$?
    trap - EXIT
    # From here on the server must be dealt with: a second Ctrl-C must not
    # stop that halfway.
    trap '' INT TERM
    if [[ -n "$CHILD_PID" ]]; then
        kill "$CHILD_PID" 2> /dev/null || true
    fi

    if $DRY_RUN; then
        log "dry run: on any exit the results are copied and the server is deleted:"
        collect_results
        delete_until_gone "${SERVER_ID:-<server-id>}" "$ZONE"
        [[ -n "$WORKDIR" ]] && rm -rf "$WORKDIR"
        exit "$rc"
    fi

    if $CREATE_REQUESTED; then
        # The create call may have gone through without this script learning
        # the ID (killed mid-request, unreadable answer). The name is unique
        # to this run, so look the server up by it.
        if [[ -z "$SERVER_ID" ]]; then
            SERVER_ID=$(scw baremetal server list "zone=$ZONE" "name=$SERVER_NAME" -o json 2> /dev/null \
                | jq -r --arg name "$SERVER_NAME" '[.[] | select(.name == $name)][0].id // empty' 2> /dev/null) || true
            [[ -n "$SERVER_ID" ]] && echo "$SERVER_ID $ZONE $SERVER_NAME" > "$STATE_FILE"
        fi
    fi

    if [[ -n "$SERVER_ID" ]]; then
        collect_results || true
        if $KEEP; then
            DELETED="kept"
            red "════════════════════════════════════════════════════════════════════"
            red "--keep: SERVER $SERVER_ID IS STILL RUNNING AND BILLED"
            red "at EUR ${HOURLY_PRICE:-$FALLBACK_HOURLY_PRICE} per hour until you delete it:"
            red "  $0 --reap --max-age 0 --server-id $SERVER_ID"
            red "  scw baremetal server delete $SERVER_ID zone=$ZONE"
            [[ -n "$SERVER_IP" ]] && red "  (ssh -i $SSH_KEY $SSH_USER@$SERVER_IP)"
            if [[ -n "$WATCHDOG_PID" ]] && kill -0 "$WATCHDOG_PID" 2> /dev/null; then
                # Tells the watchdog that this run ending is not a crash.
                touch "$WATCHDOG_DIR/$SERVER_NAME.keep"
                red "The watchdog (pid $WATCHDOG_PID) deletes it at $(date -d "@$((CREATE_EPOCH + MAX_LIFETIME_MIN * 60))" '+%H:%M %Z'),"
                red "as long as this machine stays on. Log: $WATCHDOG_DIR/$SERVER_NAME.log"
            fi
            red "════════════════════════════════════════════════════════════════════"
        elif delete_until_gone "$SERVER_ID" "$ZONE"; then
            DELETED="confirmed"
            rm -f "$STATE_FILE"
            if [[ -n "$WATCHDOG_PID" ]]; then
                kill "$WATCHDOG_PID" 2> /dev/null || true
                # Its log goes with the results: it is the record of whether
                # the watchdog had a hand in the deletion.
                mkdir -p "$RESULTS_DIR/logs"
                mv "$WATCHDOG_DIR/$SERVER_NAME.log" "$RESULTS_DIR/logs/watchdog.log" 2> /dev/null || true
            fi
        else
            DELETED="FAILED"
            print_still_exists "$SERVER_ID" "$ZONE"
            if [[ -n "$WATCHDOG_PID" ]] && kill -0 "$WATCHDOG_PID" 2> /dev/null; then
                red "The watchdog (pid $WATCHDOG_PID) keeps trying for six hours."
                red "Log: $WATCHDOG_DIR/$SERVER_NAME.log"
            fi
            rc=1
        fi
        print_cost
    elif $CREATE_REQUESTED; then
        warn "no server named $SERVER_NAME is listed; check with: scw baremetal server list zone=$ZONE"
    fi

    if [[ -n "$RESULTS_DIR" ]] && { [[ -n "$SERVER_ID" ]] || $CREATE_REQUESTED; }; then
        write_run_json "$rc" || true
        log "results: $RESULTS_DIR"
    fi
    [[ -n "$WORKDIR" ]] && rm -rf "$WORKDIR"
    exit "$rc"
}

on_signal() {
    warn "interrupted; cleaning up"
    if [[ -n "$CHILD_PID" ]]; then
        kill "$CHILD_PID" 2> /dev/null || true
    fi
    exit "$1"
}

# ── Preflight ──────────────────────────────────────────────────────────────

require_tools git jq tar base64 timeout
if ! $DRY_RUN; then
    require_tools scw curl ssh scp ssh-keygen
    require_credentials
    [[ -r "$SSH_KEY" ]] || die "SSH key $SSH_KEY not found (set SSH_KEY or --ssh-key)"
fi

REF_SHA=$(git rev-parse --verify --quiet "$REF^{commit}") || die "unknown revision: $REF"
SHORT_SHA=$(git rev-parse --short "$REF_SHA")
if [[ -n "$AGAINST" ]]; then
    AGAINST_SHA=$(git rev-parse --verify --quiet "$AGAINST^{commit}") || die "unknown revision: $AGAINST"
fi
AGAINST_TAG=""
if [[ "$MODE" == pair ]]; then
    AGAINST_TAG=$(printf '%s' "$AGAINST" | tr -c 'A-Za-z0-9._-' '_')
fi
if [[ "$REF" == HEAD ]] && [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    warn "uncommitted changes are not shipped: the server gets commit $SHORT_SHA"
fi

SERVER_NAME="monocoque-bench-$(date -u +%Y%m%d-%H%M%S)-$SHORT_SHA"
RESULTS_DIR="$RESULTS_ROOT/$(date -u +%Y-%m-%d)-$SHORT_SHA-$MODE"
WORKDIR=$(mktemp -d)

trap cleanup EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

log "mode $MODE, ref $REF ($SHORT_SHA)${AGAINST:+, against $AGAINST}, backend $BACKEND"
log "zone $ZONE, offer $OFFER, OS $OS_NAME $OS_VERSION"

# One listing proves the credentials work, finds the hourly offer and shows
# its stock. The subscription period is checked here on purpose: an offer
# name exists once per period, and the monthly one is a monthly commitment.
offers=$(scw_read "[{\"id\":\"<offer-id>\",\"name\":\"$OFFER\",\"subscription_period\":\"hourly\",\"enable\":true,\"stock\":\"available\"}]" \
    baremetal offer list "zone=$ZONE" subscription-period=hourly) \
    || die "could not list offers: check the credentials and the zone"
offer=$(jq -c --arg name "$OFFER" \
    '[.[] | select((.name | ascii_downcase) == ($name | ascii_downcase))
          | select(.subscription_period == "hourly" and .enable)][0] // empty' <<< "$offers")
[[ -n "$offer" ]] || die "no hourly offer named $OFFER in $ZONE"
OFFER_ID=$(jq -r '.id' <<< "$offer")
stock=$(jq -r '.stock' <<< "$offer")
[[ "$stock" != empty ]] || die "$OFFER is out of stock in $ZONE"
if [[ -z "$HOURLY_PRICE" ]]; then
    HOURLY_PRICE=$(jq -r '.price_per_hour
        | if type == "object" then ((.units // 0) + (.nanos // 0) / 1e9 | tostring) else empty end' <<< "$offer")
    if ! [[ "$HOURLY_PRICE" =~ ^[0-9]+(\.[0-9]+)?$ ]] || [[ "$HOURLY_PRICE" == 0 ]]; then
        HOURLY_PRICE="$FALLBACK_HOURLY_PRICE"
    fi
fi
log "offer $OFFER_ID, stock $stock, EUR $HOURLY_PRICE per hour"

existing=$(scw_read "[]" baremetal server list zone=all "tags.0=$TAG") || die "could not list servers"
existing=$(jq -r --arg tag "$TAG" \
    '.[] | select((.tags // []) | index($tag)) | "  \(.id)  \(.zone)  \(.name)  \(.status)"' <<< "$existing")
if [[ -n "$existing" ]]; then
    if $FORCE; then
        warn "servers tagged $TAG already exist (--force given):"
        printf '%s\n' "$existing" >&2
    else
        printf '%s\n' "$existing" >&2
        die "a server tagged $TAG already exists; delete it with --reap --max-age 0, or pass --force"
    fi
fi

# The OS is looked up by name and version every run: the IDs are not stable.
oses=$(scw_read "[{\"id\":\"<os-id>\",\"name\":\"$OS_NAME\",\"version\":\"$OS_VERSION LTS\",\"enabled\":true,\"cloud_init_supported\":true,\"user\":{\"default_value\":\"ubuntu\"}}]" \
    baremetal os list "zone=$ZONE" "offer-id=$OFFER_ID") || die "could not list the OS images"
os=$(jq -c --arg name "$OS_NAME" --arg version "$OS_VERSION" \
    '[.[] | select(.enabled != false and .allowed != false)
          | select((.name | ascii_downcase) == ($name | ascii_downcase))
          | select(.version | startswith($version))][0] // empty' <<< "$oses")
if [[ -z "$os" ]]; then
    jq -r '.[] | "  \(.name) \(.version)"' <<< "$oses" >&2
    die "no OS image $OS_NAME $OS_VERSION for $OFFER in $ZONE (the ones above exist)"
fi
OS_ID=$(jq -r '.id' <<< "$os")
[[ -n "$SSH_USER" ]] || SSH_USER=$(jq -r '.user.default_value // "ubuntu"' <<< "$os")
cloud_init=$(jq -r '.cloud_init_supported // false' <<< "$os")
log "OS $(jq -r '"\(.name) \(.version)"' <<< "$os") is $OS_ID, login user $SSH_USER, cloud-init $cloud_init"

# The key installed on the server is the registered one whose public half
# matches the private key this script will log in with.
if [[ -z "$SSH_KEY_ID" ]]; then
    if $DRY_RUN; then
        show ssh-keygen -y -f "$SSH_KEY"
        show scw iam ssh-key list "project-id=\$SCW_DEFAULT_PROJECT_ID" -o json
        SSH_KEY_ID="<ssh-key-id>"
    else
        public=$(ssh-keygen -y -f "$SSH_KEY" 2> /dev/null | awk '{print $2}') \
            || die "could not read $SSH_KEY (a passphrase-protected key needs an agent-less copy)"
        keys=$(scw iam ssh-key list "project-id=$SCW_DEFAULT_PROJECT_ID" -o json) || die "could not list SSH keys"
        SSH_KEY_ID=$(jq -r --arg public "$public" \
            '[.[] | select(.disabled != true) | select((.public_key | split(" ")[1]) == $public)][0].id // empty' <<< "$keys")
        [[ -n "$SSH_KEY_ID" ]] || die "the public key of $SSH_KEY is not registered in the Scaleway project"
    fi
fi

# ── Source to ship ─────────────────────────────────────────────────────────

log "packing the source"
git archive --format=tar.gz -o "$WORKDIR/src.tar.gz" "$REF_SHA"
if [[ "$MODE" == pair ]]; then
    git archive --format=tar.gz -o "$WORKDIR/against.tar.gz" "$AGAINST_SHA"
fi
{
    printf 'MODE=%q\n' "$MODE"
    printf 'BACKEND=%q\n' "$BACKEND"
    printf 'ROUNDS=%q\n' "$ROUNDS"
    printf 'PIN=%q\n' "${PIN:-2,4}"
    printf 'BENCHES=%q\n' "${BENCHES[*]:-}"
    printf 'AGAINST_TAG=%q\n' "$AGAINST_TAG"
    printf 'ARENA_CMD=%q\n' "$ARENA_CMD"
    printf 'MODE_TIMEOUT_MIN=%q\n' "$MODE_TIMEOUT_MIN"
} > "$WORKDIR/run.env"
run_script > "$WORKDIR/run.sh"
prepare_script > "$WORKDIR/prepare.sh"
bench_tune_script > "$WORKDIR/bench-tune"
cloud_config "$SSH_USER" > "$WORKDIR/user-data.yaml"

# ── Create ─────────────────────────────────────────────────────────────────

# The create call goes straight to the API, not through `scw baremetal server
# create`, for two reasons checked against scw 2.65.2: that command rejects
# `user-data` although its help lists it, and it turns `type=<name>` into the
# first offer with that name without looking at the subscription period. Here
# the offer is the hourly one resolved above, by ID. The secret key reaches
# curl through a config on stdin, so it is never in a process listing.
body=$(jq -n \
    --arg offer "$OFFER_ID" --arg project "${SCW_DEFAULT_PROJECT_ID:-<project-id>}" \
    --arg name "$SERVER_NAME" --arg tag "$TAG" --arg os "$OS_ID" --arg key "$SSH_KEY_ID" \
    --arg description "monocoque benchmark run, $MODE, $SHORT_SHA. Safe to delete." \
    --arg user_data "$(base64 -w0 < "$WORKDIR/user-data.yaml")" \
    '{offer_id: $offer, project_id: $project, name: $name, description: $description,
      tags: [$tag], user_data: $user_data,
      install: {os_id: $os, hostname: $name, ssh_key_ids: [$key]}}')
create_url="$API_URL/baremetal/v1/zones/$ZONE/servers"

if $DRY_RUN; then
    log "dry run: the cloud-init user data would be:"
    sed 's/^/    /' "$WORKDIR/user-data.yaml" >&2
    show curl --silent --show-error --fail-with-body -X POST \
        -H "X-Auth-Token: <SCW_SECRET_KEY>" -H "Content-Type: application/json" \
        --data "$(jq -c '.user_data = "<base64 of the user data above>"' <<< "$body")" "$create_url"
    show setsid nohup "$0" --watchdog "<now + ${MAX_LIFETIME_MIN} min>" "$SERVER_NAME" "$ZONE" "$$"
    SERVER_ID="<server-id>"
    SERVER_IP="<server-ip>"
else
    CREATE_EPOCH=$(date -u +%s)
    # Started before the create call, so there is no moment at which a server
    # exists and nothing outside this process knows it has to go.
    mkdir -p "$WATCHDOG_DIR"
    SCW_BENCH_TAG="$TAG" setsid nohup "$0" --watchdog "$((CREATE_EPOCH + MAX_LIFETIME_MIN * 60))" \
        "$SERVER_NAME" "$ZONE" "$$" > "$WATCHDOG_DIR/$SERVER_NAME.log" 2>&1 < /dev/null &
    WATCHDOG_PID=$!
    log "watchdog $WATCHDOG_PID deletes the server after $MAX_LIFETIME_MIN minutes at the latest"

    log "creating $SERVER_NAME"
    CREATE_REQUESTED=true
    response=$(printf 'header = "X-Auth-Token: %s"\n' "$SCW_SECRET_KEY" \
        | curl --silent --show-error --fail-with-body --config - -X POST \
            -H "Content-Type: application/json" --data "$body" "$create_url") \
        || die "the server was not created: $response"
    SERVER_ID=$(jq -r '.id // empty' <<< "$response")
    [[ -n "$SERVER_ID" ]] || die "the create call returned no server ID"
    # Written before anything else can fail, so a crash leaves a pointer.
    echo "$SERVER_ID $ZONE $SERVER_NAME" > "$STATE_FILE"
    log "server $SERVER_ID created; its ID is in $STATE_FILE"

    # A last check that this is the hourly offer. If it is not, the exit
    # deletes the server at once.
    created_offer=$(jq -r '.offer_id // empty' <<< "$response")
    [[ "$created_offer" == "$OFFER_ID" ]] \
        || die "the server was created on offer $created_offer, not the hourly $OFFER_ID"
fi

# ── Wait for it ────────────────────────────────────────────────────────────

log "waiting for delivery and installation (up to $READY_TIMEOUT_MIN minutes)"
show scw baremetal server wait "$SERVER_ID" "zone=$ZONE" "timeout=${READY_TIMEOUT_MIN}m"
if ! $DRY_RUN; then
    interruptible scw baremetal server wait "$SERVER_ID" "zone=$ZONE" "timeout=${READY_TIMEOUT_MIN}m" \
        > "$WORKDIR/wait.log" 2>&1 || {
        tail -5 "$WORKDIR/wait.log" >&2
        die "the server did not become ready"
    }
    READY_EPOCH=$(date -u +%s)
    log "ready after $(((READY_EPOCH - CREATE_EPOCH) / 60)) minutes"
fi

server=$(scw_read '{"status":"ready","install":{"status":"completed"},"ips":[{"version":"IPv4","address":"<server-ip>"}]}' \
    baremetal server get "$SERVER_ID" "zone=$ZONE") || die "could not read the server back"
[[ "$(jq -r '.status' <<< "$server")" == ready ]] || die "server status is $(jq -r '.status' <<< "$server")"
[[ "$(jq -r '.install.status' <<< "$server")" == completed ]] \
    || die "installation status is $(jq -r '.install.status' <<< "$server")"
SERVER_IP=$(jq -r '[.ips[] | select(.version == "IPv4")][0].address // .ips[0].address // empty' <<< "$server")
[[ -n "$SERVER_IP" ]] || die "the server has no IP address"
log "server address $SERVER_IP"

if ! $DRY_RUN; then
    log "waiting for SSH"
    ssh_deadline=$(($(date -u +%s) + 900))
    until remote 20 true 2> /dev/null; do
        check_hard_cap
        (($(date -u +%s) < ssh_deadline)) || die "SSH did not come up within 15 minutes"
        pause 10
    done
fi

log "waiting for cloud-init"
# A non-zero status here is not a failure by itself: cloud-init also returns
# one when it finished with warnings. What matters is whether the server got
# prepared, and prepare.sh below checks exactly that and does what is missing.
remote 2400 'cloud-init status --wait > /dev/null 2>&1' \
    || log "cloud-init ended with a non-zero status; the preparation is checked over SSH next"
check_hard_cap

# ── Prepare, upload, run ───────────────────────────────────────────────────

remote 60 'mkdir -p ~/bench'
for file in src.tar.gz run.env run.sh prepare.sh bench-tune; do
    upload "$WORKDIR/$file" "bench/$file"
done
if [[ "$MODE" == pair ]]; then
    upload "$WORKDIR/against.tar.gz" "bench/against.tar.gz"
fi
# Does nothing where cloud-init already did the work.
remote 1800 'bash ~/bench/prepare.sh' || die "could not prepare the server"

log "building and running $MODE (up to $MODE_TIMEOUT_MIN minutes for the benchmark)"
# The redirections have to sit on the one backgrounded command. On a list
# such as `cd dir && cmd &` the shell forks a copy of itself to run the list,
# that copy keeps the SSH session's output open for as long as the run
# lasts, and ssh then never returns.
# shellcheck disable=SC2016
remote 60 'setsid nohup bash "$HOME/bench/run.sh" > /dev/null 2>&1 < /dev/null &'

if ! $DRY_RUN; then
    last_stage=""
    while true; do
        check_hard_cap
        pause 30
        # shellcheck disable=SC2016
        state=$(remote 30 'cd ~/bench/logs 2>/dev/null && echo "$(cat stage 2>/dev/null)|$(cat build.exit 2>/dev/null)|$(cat mode.exit 2>/dev/null)"' 2> /dev/null) || continue
        stage="${state%%|*}"
        rest="${state#*|}"
        BUILD_EXIT="${rest%%|*}"
        MODE_EXIT="${rest#*|}"
        if [[ "$stage" != "$last_stage" ]]; then
            log "stage: $stage"
            last_stage="$stage"
        fi
        if [[ -n "$BUILD_EXIT" && "$BUILD_EXIT" != 0 ]]; then
            die "the build failed with exit code $BUILD_EXIT (see logs/build.log)"
        fi
        if [[ -n "$MODE_EXIT" ]]; then
            break
        fi
    done
    if [[ "$MODE_EXIT" == 124 ]]; then
        die "$MODE ran past its $MODE_TIMEOUT_MIN minute limit"
    elif [[ "$MODE_EXIT" != 0 ]]; then
        die "$MODE failed with exit code $MODE_EXIT (see logs/mode.log)"
    fi
    log "$MODE finished"
fi

# The exit trap copies the results, deletes the server and reports the cost.
exit 0
