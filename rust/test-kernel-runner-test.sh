#!/bin/sh
# Exact selection, process isolation, bounded overlap and failure/signal joining.
# These fixtures run unprivileged and use no kernel objects or runtime stores.
set -eu
root=$(mktemp -d)
controller=
# shellcheck disable=SC2329
finish() {
    status=$?
    trap - EXIT
    if [ -n "$controller" ]; then kill -TERM "$controller" 2>/dev/null || true; wait "$controller" || true; fi
    rm -rf "$root"
    exit "$status"
}
trap finish EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
export FAKE_KERNEL_LOG="$root/calls"
export FAKE_TEST_LIST="$root/list"
export TMPDIR="$root/tmp"
mkdir "$TMPDIR"
cat > "$root/kernel" <<'MOCK'
#!/bin/sh
set -eu
if [ "$1" = --list ]; then cat "$FAKE_TEST_LIST"; exit; fi
[ "$#" -eq 3 ] && [ "$1" = --exact ] && [ "$2" = --test-threads=1 ]
name=$3
if [ -n "${FAKE_ACTIVITY:-}" ]; then
    exec 8> "$FAKE_ACTIVITY/lock"
    flock -x 8
    read -r active peak < "$FAKE_ACTIVITY/counts"
    active=$((active + 1))
    if [ "$active" -gt "$peak" ]; then peak=$active; fi
    printf '%s %s\n' "$active" "$peak" > "$FAKE_ACTIVITY/counts"
    flock -u 8
    # shellcheck disable=SC2329
    leave() {
        flock -x 8
        read -r active peak < "$FAKE_ACTIVITY/counts"
        printf '%s %s\n' "$((active - 1))" "$peak" > "$FAKE_ACTIVITY/counts"
        flock -u 8
    }
    trap leave EXIT
    sleep 0.01
fi
printf '%s %s\n' "$name" "$$" >> "$FAKE_KERNEL_LOG"
if [ -n "${FAKE_BARRIER:-}" ]; then
    case "$name" in
        sqlite_case|sqlite_case_longer)
            # Every case owns a distinct descriptor. Its children must never
            # inherit the other concurrently running case's descriptor.
            exec 9> "$FAKE_BARRIER/$name.owned"
            printf '%s\n' "$$" > "$FAKE_BARRIER/$name.started"
            trap 'printf "%s\n" "$$" > "$FAKE_BARRIER/$name.stopped"; exit 143' TERM
            while [ ! -f "$FAKE_BARRIER/release" ]; do sleep 0.01; done
            for file in /proc/self/fd/*; do
                target=$(readlink "$file" 2>/dev/null || true)
                case "$target" in
                    "$FAKE_BARRIER/"*.owned) [ "$target" = "$FAKE_BARRIER/$name.owned" ] ;;
                esac
            done
            ;;
    esac
fi
if [ "$name" = "${FAKE_FAILURE:-never-match}" ]; then exit 7; fi
if [ "$name" = "${FAKE_CRASH:-never-match}" ]; then kill -KILL "$$"; fi
if [ "$name" = "${FAKE_EMPTY:-never-match}" ]; then
    printf 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.00s\n'
    exit
fi
if [ "$name" = "${FAKE_IGNORED:-never-match}" ]; then
    printf 'test %s ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 8 filtered out; finished in 0.00s\n' "$name"
    exit
fi
printf 'test %s ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.00s\n' "$name"
MOCK
chmod +x "$root/kernel"
cat > "$FAKE_TEST_LIST" <<'LIST'
sqlite_script_corpus: test
sqlite_attached_unload_failures_and_cancellation: test
sqlite_cli_effect_boundaries: test
sqlite_lsm_native_link_recovery: test
sqlite_xdp_load_lifecycle: test
sqlite_uprobe_native_link_recovery: test
sqlite_kprobe_native_link_recovery: test
sqlite_map_sharing_native_recovery: test
sqlite_case: test
sqlite_case_longer: test
go_case: test
LIST
runner="${0%/*}/test-kernel.sh"
sed 's/: test$//' "$FAKE_TEST_LIST" | sort > "$root/expected"
FAKE_ACTIVITY="$root/activity"; export FAKE_ACTIVITY
mkdir "$FAKE_ACTIVITY"
for workers in 1 2 4 8 ''; do
    printf '0 0\n' > "$FAKE_ACTIVITY/counts"
    : > "$FAKE_KERNEL_LOG"
    BPFMAN_KERNEL_TEST_THREADS=$workers BPFMAN_KERNEL_SUITE=all sh "$runner" "$root/kernel" > "$root/output"
    cut -d ' ' -f 1 "$FAKE_KERNEL_LOG" | sort > "$root/actual"
    cmp "$root/expected" "$root/actual"
    # Every selected case ran in a different process, with no duplicate case.
    [ "$(cut -d ' ' -f 2 "$FAKE_KERNEL_LOG" | sort -u | wc -l)" -eq 11 ]
    grep -Fq 'discovered=11' "$root/output"
    grep -Fq 'selected=11' "$root/output"
    read -r active peak < "$FAKE_ACTIVITY/counts"
    [ "$active" -eq 0 ] && [ "$peak" -le "${workers:-8}" ]
    [ -z "$(ls -A "$TMPDIR")" ]
done
unset FAKE_ACTIVITY
: > "$FAKE_KERNEL_LOG"
BPFMAN_KERNEL_TEST_THREADS=2 BPFMAN_KERNEL_SUITE=contracts sh "$runner" "$root/kernel" > "$root/output"
sed '/_script_corpus$/d' "$root/expected" > "$root/contracts"
cut -d ' ' -f 1 "$FAKE_KERNEL_LOG" | sort > "$root/actual"
cmp "$root/contracts" "$root/actual"

# Invalid admission has no effects; empty and duplicate discovery also fail.
for invalid in 0 3 16 invalid; do
    : > "$FAKE_KERNEL_LOG"
    if BPFMAN_KERNEL_TEST_THREADS=$invalid sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
    [ ! -s "$FAKE_KERNEL_LOG" ]
done
: > "$FAKE_KERNEL_LOG"
if BPFMAN_KERNEL_SUITE=unknown sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
if sh "$runner" "$root/kernel" unexpected_filter > "$root/output" 2>&1; then exit 1; fi
[ ! -s "$FAKE_KERNEL_LOG" ]
cp "$FAKE_TEST_LIST" "$root/original-list"
for listing in empty duplicate unclassified invalid-name; do
    case "$listing" in
        empty) : > "$FAKE_TEST_LIST" ;;
        duplicate) cat "$root/original-list" "$root/original-list" > "$FAKE_TEST_LIST" ;;
        unclassified) printf 'unclassified: test\n' > "$FAKE_TEST_LIST" ;;
        invalid-name) printf 'sqlite_bad/name: test\n' > "$FAKE_TEST_LIST" ;;
    esac
    if sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
    [ ! -s "$FAKE_KERNEL_LOG" ]
done
cp "$root/original-list" "$FAKE_TEST_LIST"

# Drain the complete current stage on failures, crashes, empty or ignored cases;
# never report success or advance into the later Go stage.
for failure in FAKE_FAILURE FAKE_CRASH FAKE_EMPTY FAKE_IGNORED; do
    : > "$FAKE_KERNEL_LOG"
    if env "$failure=sqlite_case" BPFMAN_KERNEL_TEST_THREADS=2 sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
    sed '/^go_case$/d' "$root/expected" > "$root/failed-expected"
    cut -d ' ' -f 1 "$FAKE_KERNEL_LOG" | sort > "$root/actual"
    cmp "$root/failed-expected" "$root/actual"
    [ -z "$(ls -A "$TMPDIR")" ]
done

# A handshake proves actual overlap. No timing assumption establishes success.
wait_for_pair() {
    attempts=0
    while [ ! -f "$FAKE_BARRIER/sqlite_case.started" ] || [ ! -f "$FAKE_BARRIER/sqlite_case_longer.started" ]; do
        attempts=$((attempts + 1))
        [ "$attempts" -lt 500 ] || { echo 'worker pair did not start' >&2; exit 1; }
        sleep 0.01
    done
    read -r first < "$FAKE_BARRIER/sqlite_case.started"
    read -r second < "$FAKE_BARRIER/sqlite_case_longer.started"
    [ "$first" != "$second" ]
}
printf 'sqlite_case: test\nsqlite_case_longer: test\n' > "$FAKE_TEST_LIST"
for action in release signal; do
    FAKE_BARRIER="$root/$action"; export FAKE_BARRIER
    mkdir "$FAKE_BARRIER"
    BPFMAN_KERNEL_TEST_THREADS=2 sh "$runner" "$root/kernel" > "$root/output" 2>&1 &
    controller=$!
    wait_for_pair
    case "$action" in
        release)
            : > "$FAKE_BARRIER/release"
            wait "$controller"
            ;;
        signal)
            kill -TERM "$controller"
            if wait "$controller"; then exit 1; else [ "$?" -eq 143 ]; fi
            [ -f "$FAKE_BARRIER/sqlite_case.stopped" ]
            [ -f "$FAKE_BARRIER/sqlite_case_longer.stopped" ]
            ;;
    esac
    controller=
    if kill -0 "$first" 2>/dev/null || kill -0 "$second" 2>/dev/null; then
        echo 'case process survived controller completion' >&2; exit 1
    fi
    [ -z "$(ls -A "$TMPDIR")" ]
done
printf 'kernel runner scheduling contracts passed\n'
