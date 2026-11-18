#!/usr/bin/env bash
#
# scaleway_bench.sh against stand-ins for scw, curl and sleep: nothing here
# talks to Scaleway and nothing is rented.
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

cat > "$TMP/bin/scw" << 'EOF'
#!/usr/bin/env bash
# $FAKE/servers.json is what a listing returns, less whatever was deleted.
# $FAKE/hide holds a number: that many listings come back empty first.
case "$*" in
    "baremetal offer list"*)
        echo '[{"id":"offer-1","name":"EM-B130E-NVME","subscription_period":"hourly","enable":true,"stock":"available","price_per_hour":{"units":0,"nanos":247000000}}]' ;;
    "baremetal os list"*)
        echo '[{"id":"os-1","name":"Ubuntu","version":"26.04 LTS","enabled":true,"cloud_init_supported":true,"user":{"default_value":"ubuntu"}}]' ;;
    "iam ssh-key list"*)
        printf '[{"id":"key-1","public_key":"%s"}]\n' "$(cat "$FAKE/public_key")" ;;
    "baremetal server list"*)
        echo "$*" >> "$FAKE/listings.log"
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

# The create call "goes through" (the server is in servers.json already) and
# the answer is lost: curl gives up as on a reset connection.
printf '#!/bin/sh\ncat > /dev/null\nexit 56\n' > "$TMP/bin/curl"
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

iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
NOW=$(date -u +%s)
HOUR=3600

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

fresh unreadable
echo '[{"id":"odd-date","zone":"z","name":"a","tags":["monocoque-bench"],"created_at":"yesterday"}]' \
    > "$FAKE/servers.json"
"$SCRIPT" --reap --max-age 4h > "$FAKE/out.log" 2>&1
check "an unreadable creation time fails the reap" test $? -ne 0
check "and the server is deleted all the same" deleted odd-date

fresh by-name
jq -n --arg young "$(iso $((NOW - 60)))" --arg later "monocoque-bench-expires-$((NOW + HOUR))" '[
    {id: "wanted", zone: "z", name: "monocoque-bench-run-1", tags: ["monocoque-bench", $later], created_at: $young},
    {id: "other",  zone: "z", name: "monocoque-bench-run-2", tags: ["monocoque-bench", $later], created_at: $young}
]' > "$FAKE/servers.json"
echo 3 > "$FAKE/hide"
"$SCRIPT" --reap --max-age 0 --server-name monocoque-bench-run-1 > "$FAKE/out.log" 2>&1
check "a reap by name waits for a server that is not listed yet" deleted wanted
check "and deletes only the one it was asked for" not_deleted other

# ── A create call whose answer is lost ─────────────────────────────────────

fresh lost-create
REPO="$TMP/repo"
mkdir -p "$REPO"
(
    cd "$REPO" || exit 1
    git init -q -b main
    git -c user.email=t@localhost -c user.name=t commit -q --allow-empty -m one
)
command ssh-keygen -q -t ed25519 -N '' -f "$TMP/key" > /dev/null
awk '{print $1, $2}' "$TMP/key.pub" > "$FAKE/public_key"

# The server exists from the start (the call did go through), and the first
# listings do not show it yet. The preflight makes one listing of its own.
(
    cd "$REPO" || exit 1
    name_file="$FAKE/name"
    # The name is made by the script, so the fake listing is written by a
    # scw wrapper the moment the script first asks for the offers.
    cat > "$TMP/bin/scw-real" < "$TMP/bin/scw"
    cat > "$TMP/bin/scw" << EOF
#!/usr/bin/env bash
if [[ "\$*" == *"name=monocoque-bench-"* && ! -s "$name_file" ]]; then
    for arg in "\$@"; do
        case "\$arg" in name=*) echo "\${arg#name=}" > "$name_file" ;; esac
    done
    jq -n --arg name "\$(cat "$name_file")" --arg at "$(iso "$NOW")" \\
        '[{id: "lost-server", zone: "fr-par-1", name: \$name, tags: ["monocoque-bench"], created_at: \$at}]' \\
        > "\$FAKE/servers.json"
    echo 3 > "\$FAKE/hide"
fi
exec bash "$TMP/bin/scw-real" "\$@"
EOF
    chmod +x "$TMP/bin/scw"
    SSH_KEY="$TMP/key" XDG_STATE_HOME="$TMP/state" "$SCRIPT" --mode suite > "$FAKE/out.log" 2>&1
    echo $? > "$FAKE/exit"
)
check "a lost create answer makes the run fail" test "$(cat "$FAKE/exit")" -ne 0
check "the server is still found by name and deleted" deleted lost-server
check "the state file is gone once the server is" test ! -e "$REPO/.scaleway-bench-server"
check "the exit trap looked more than once" test "$(grep -c 'looking again' "$FAKE/out.log")" -ge 1

if ((FAILED > 0)); then
    echo "$FAILED check(s) failed; the last output:"
    tail -20 "$FAKE/out.log"
    exit 1
fi
echo "all checks passed"
