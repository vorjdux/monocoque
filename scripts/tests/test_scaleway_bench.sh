#!/usr/bin/env bash
#
# scaleway_bench.sh against stand-ins for scw, curl, ssh and sleep: nothing
# here talks to Scaleway and nothing is rented.
#
#   scripts/tests/test_scaleway_bench.sh
#
# The stand-in scw keeps its "servers" in a JSON file and a log of every
# delete it was asked for, which is what the checks below read.

set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/scaleway_bench.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
FAILED=0

ok() { printf 'ok    %s\n' "$1"; }
bad() {
    printf 'FAIL  %s\n' "$1"
    FAILED=$((FAILED + 1))
}
check() {
    local what="$1"
    shift
    if "$@"; then ok "$what"; else bad "$what"; fi
}

# ── Stand-ins ──────────────────────────────────────────────────────────────

mkdir -p "$TMP/bin"

# No waiting in a test.
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/sleep"

# Never reached by these cases, but the script asks for them to exist.
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/ssh"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/scp"
# `ssh-keygen -y -f KEY` prints the public half of the key.
printf '#!/bin/sh\necho "ssh-ed25519 AAAAstandin test"\n' > "$TMP/bin/ssh-keygen"

cat > "$TMP/bin/scw" << 'EOF'
#!/usr/bin/env bash
# $FAKE/servers.json is what a listing returns, less whatever was deleted.
# $FAKE/hide holds a number: that many listings come back empty first.
# $FAKE/lost-create, if there, makes a server appear under the name the
# script first asks for: its create call "went through" unseen.
case "$*" in
    "baremetal offer list"*)
        echo '[{"id":"offer-1","name":"EM-B130E-NVME","subscription_period":"hourly","enable":true,"stock":"available","price_per_hour":{"units":0,"nanos":247000000}}]' ;;
    "baremetal os list"*)
        echo '[{"id":"os-1","name":"Ubuntu","version":"26.04 LTS","enabled":true,"cloud_init_supported":true,"user":{"default_value":"ubuntu"}}]' ;;
    "iam ssh-key list"*)
        echo '[{"id":"key-1","public_key":"ssh-ed25519 AAAAstandin someone@somewhere"}]' ;;
    "baremetal server list"*)
        if [[ -e "$FAKE/lost-create" && ! -s "$FAKE/name" ]]; then
            for arg in "$@"; do
                case "$arg" in name=*) echo "${arg#name=}" > "$FAKE/name" ;; esac
            done
            if [[ -s "$FAKE/name" ]]; then
                jq -n --arg name "$(cat "$FAKE/name")" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
                    '[{id: "lost-server", zone: "fr-par-1", name: $name, tags: ["monocoque-bench"], created_at: $at}]' \
                    > "$FAKE/servers.json"
                echo 3 > "$FAKE/hide"
            fi
        fi
        hide=$(cat "$FAKE/hide" 2> /dev/null || echo 0)
        if ((hide > 0)); then
            echo $((hide - 1)) > "$FAKE/hide"
            echo '[]'
            exit 0
        fi
        deleted=$(jq -R . < "$FAKE/deleted.log" 2> /dev/null | jq -s . || echo '[]')
        jq --argjson gone "${deleted:-[]}" 'map(select(.id as $id | $gone | index($id) | not))' "$FAKE/servers.json" ;;
    "baremetal server delete"*)
        echo "$4" >> "$FAKE/deleted.log" ;;
    *)
        echo "unexpected scw call: $*" >&2
        exit 1 ;;
esac
EOF

# What the create call does is set per case, in $FAKE/curl.
cat > "$TMP/bin/curl" << 'EOF'
#!/bin/sh
cat > /dev/null
. "$FAKE/curl"
EOF
chmod +x "$TMP/bin"/*

export PATH="$TMP/bin:$PATH"
export SCW_ACCESS_KEY=x SCW_SECRET_KEY=x SCW_DEFAULT_PROJECT_ID=x SCW_DEFAULT_ORGANIZATION_ID=x

# A fresh "account" for each case.
fresh() {
    export FAKE="$TMP/fake-$1"
    mkdir -p "$FAKE"
    : > "$FAKE/deleted.log"
    echo '[]' > "$FAKE/servers.json"
}
deleted() { grep -qx "$1" "$FAKE/deleted.log"; }
not_deleted() { ! deleted "$1"; }
said() { grep -q "$1" "$FAKE/out.log"; }

iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
NOW=$(date -u +%s)
HOUR=3600

# The script reads and writes its state file in the directory it runs in.
WORK="$TMP/work"
mkdir -p "$WORK"
cd "$WORK" || exit 1

# ── The reaper and a server's own deadline ─────────────────────────────────

fresh reap
jq -n --arg old "$(iso $((NOW - 5 * HOUR)))" --arg ancient "$(iso $((NOW - 25 * HOUR)))" \
    --arg young "$(iso $((NOW - HOUR)))" \
    --arg later "monocoque-bench-expires-$((NOW + HOUR))" \
    --arg earlier "monocoque-bench-expires-$((NOW - 60))" '[
    {id: "no-deadline-old",   zone: "z", name: "a", tags: ["monocoque-bench"], created_at: $old},
    {id: "no-deadline-young", zone: "z", name: "b", tags: ["monocoque-bench"], created_at: $young},
    {id: "deadline-ahead",    zone: "z", name: "c", tags: ["monocoque-bench", $later], created_at: $old},
    {id: "deadline-passed",   zone: "z", name: "d", tags: ["monocoque-bench", $earlier], created_at: $young},
    {id: "deadline-ancient",  zone: "z", name: "e", tags: ["monocoque-bench", $later], created_at: $ancient},
    {id: "not-ours",          zone: "z", name: "f", tags: ["something-else"], created_at: $ancient}
]' > "$FAKE/servers.json"

"$SCRIPT" --reap --max-age 4h > "$FAKE/out.log" 2>&1
check "reap exits 0 when everything due is gone" test $? -eq 0
check "a server without a deadline goes at --max-age" deleted no-deadline-old
check "a younger one without a deadline stays" not_deleted no-deadline-young
check "a run still inside its own deadline is left alone past --max-age" not_deleted deadline-ahead
check "a server past its own deadline goes" deleted deadline-passed
check "a deadline does not keep a server past the backstop" deleted deadline-ancient
check "a server without the tag is never touched" not_deleted not-ours

"$SCRIPT" --reap --max-age 0 > "$FAKE/out.log" 2>&1
check "--max-age 0 deletes a server whatever its deadline" deleted deadline-ahead
check "--max-age 0 still leaves an untagged server" not_deleted not-ours

fresh octal
jq -n --arg old "$(iso $((NOW - 9 * HOUR)))" \
    '[{id: "nine-hours", zone: "z", name: "a", tags: ["monocoque-bench"], created_at: $old}]' \
    > "$FAKE/servers.json"
"$SCRIPT" --reap --max-age 08h > "$FAKE/out.log" 2>&1
check "--max-age 08h is eight hours, not an error" test $? -eq 0
check "and the nine-hour-old server goes" deleted nine-hours

# ── A creation time that cannot be read ────────────────────────────────────

fresh unreadable
jq -n --arg later "monocoque-bench-expires-$((NOW + HOUR))" \
    --arg earlier "monocoque-bench-expires-$((NOW - 60))" '[
    {id: "odd-no-deadline",     zone: "z", name: "a", tags: ["monocoque-bench"], created_at: "yesterday"},
    {id: "odd-deadline-ahead",  zone: "z", name: "b", tags: ["monocoque-bench", $later], created_at: null},
    {id: "odd-deadline-passed", zone: "z", name: "c", tags: ["monocoque-bench", $earlier], created_at: "yesterday"}
]' > "$FAKE/servers.json"
"$SCRIPT" --reap --max-age 4h > "$FAKE/out.log" 2>&1
check "an unreadable creation time fails the reap" test $? -ne 0
check "with no deadline the server is deleted all the same" deleted odd-no-deadline
check "with a deadline still ahead it is kept" not_deleted odd-deadline-ahead
check "with a deadline behind it is deleted" deleted odd-deadline-passed

# ── A reap by name ─────────────────────────────────────────────────────────

fresh by-name
jq -n --arg young "$(iso $((NOW - 60)))" --arg later "monocoque-bench-expires-$((NOW + HOUR))" '[
    {id: "wanted", zone: "z", name: "monocoque-bench-run-1", tags: ["monocoque-bench", $later], created_at: $young},
    {id: "other",  zone: "z", name: "monocoque-bench-run-2", tags: ["monocoque-bench", $later], created_at: $young}
]' > "$FAKE/servers.json"
echo 3 > "$FAKE/hide"
"$SCRIPT" --reap --max-age 0 --server-name monocoque-bench-run-1 > "$FAKE/out.log" 2>&1
check "a reap by name waits for a server that is not listed yet" deleted wanted
check "and deletes only the one it was asked for" not_deleted other

# ── The state file belongs to one server ───────────────────────────────────

fresh state-file
jq -n --arg old "$(iso $((NOW - 5 * HOUR)))" '[
    {id: "abc",     zone: "z", name: "",       tags: ["monocoque-bench"], created_at: $old},
    {id: "abc-123", zone: "z", name: "run-12", tags: ["monocoque-bench"], created_at: $old}
]' > "$FAKE/servers.json"
echo "abc-1234 z run-1" > .scaleway-bench-server
"$SCRIPT" --reap --max-age 4h > "$FAKE/out.log" 2>&1
check "a server with an empty name is still reaped" deleted abc
check "a state file for another server survives an empty name and a like ID" test -s .scaleway-bench-server
echo "pending z run-12" > .scaleway-bench-server
fresh state-file-pending
jq -n --arg old "$(iso $((NOW - 5 * HOUR)))" \
    '[{id: "abc-123", zone: "z", name: "run-12", tags: ["monocoque-bench"], created_at: $old}]' \
    > "$FAKE/servers.json"
"$SCRIPT" --reap --max-age 4h > "$FAKE/out.log" 2>&1
check "a pending state file goes with the server of that name" test ! -e .scaleway-bench-server
rm -f .scaleway-bench-server

# ── A create call whose answer is lost ─────────────────────────────────────

REPO="$TMP/repo"
mkdir -p "$REPO"
cd "$REPO" || exit 1
git init -q -b main
git -c user.email=t@localhost -c user.name=t commit -q --allow-empty -m one
echo "not a real key" > "$TMP/key"

# The server exists from the moment the script first looks for it by name
# (the call did go through), and the first listings do not show it.
lost_create() {
    fresh "$1"
    touch "$FAKE/lost-create"
    echo "$2" > "$FAKE/curl"
    SSH_KEY="$TMP/key" XDG_STATE_HOME="$TMP/state" "$SCRIPT" --mode suite > "$FAKE/out.log" 2>&1
    echo $? > "$FAKE/exit"
}

lost_create reset-connection 'exit 56'
check "the run got as far as the create call" said "creating monocoque-bench-"
check "a lost create answer makes the run fail" test "$(cat "$FAKE/exit")" -ne 0
check "the server is still found by name and deleted" deleted lost-server
check "the state file is gone once the server is" test ! -e .scaleway-bench-server
check "the exit trap looked more than once" said "looking again"

lost_create proxy-timeout 'printf "upstream timed out\n408"'
check "the run got as far as the create call (408)" said "creating monocoque-bench-"
check "a 408 leaves the outcome open, so the server is looked for" said "looking again"
check "and it is found and deleted" deleted lost-server

# A plain refusal is certain: nothing was created, nothing to wait for.
fresh refused
printf '%s\n' 'printf "out of stock\n400"' > "$FAKE/curl"
SSH_KEY="$TMP/key" XDG_STATE_HOME="$TMP/state" "$SCRIPT" --mode suite > "$FAKE/out.log" 2>&1
check "a 400 fails the run" test $? -ne 0
check "the run got as far as the create call (400)" said "creating monocoque-bench-"
check "a 400 is not followed by a search" bash -c "! grep -q 'looking again' '$FAKE/out.log'"
check "and leaves no state file" test ! -e .scaleway-bench-server

if ((FAILED > 0)); then
    echo "$FAILED check(s) failed; the last output:"
    tail -20 "$FAKE/out.log"
    exit 1
fi
echo "all checks passed"
