#!/bin/sh
# A worker owns no BPF descriptors: each exact case executes in a fresh process.
set -eu
binary=${1:?Kernel test binary required}
results=${2:?Result directory required}
queue=${3:?Case queue required}
child=

# Signal the current case and reap it before this worker exits. The controller
# also signals the worker's process group to reach ordinary case descendants.
# shellcheck disable=SC2329
stop() {
    trap '' HUP INT TERM
    if [ -n "$child" ]; then
        kill -"$1" "$child" 2>/dev/null || true
        wait "$child" || true
    fi
    exit "$2"
}
trap 'stop HUP 129' HUP
trap 'stop INT 130' INT
trap 'stop TERM 143' TERM

failed=0
while IFS= read -r name <&3; do
    output="$results/$name.log"
    "$binary" --exact --test-threads=1 "$name" > "$output" 2>&1 3<&- &
    child=$!
    pid=$child
    if wait "$child"; then status=0; else status=$?; fi
    child=

    # An exact filter can exit zero after selecting no tests, or an ignored
    # test. Require one actual pass, preserving failures and abnormal exits.
    if [ "$status" -eq 0 ]; then
        if ! grep -Fxq "test $name ... ok" "$output" ||
           ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in ' "$output"; then
            printf 'kernel case did not report exactly one passing test: %s\n' "$name" >> "$output"
            status=1
        fi
    fi
    printf '%s %s\n' "$status" "$pid" > "$results/$name.result"
    if [ "$status" -ne 0 ]; then failed=1; fi
done 3< "$queue"
exit "$failed"
