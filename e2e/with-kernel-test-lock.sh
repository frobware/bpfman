#!/bin/sh
# Hold the host-wide gate across module setup, test execution and cleanup.
# The Go binaries also take their own lock, including when launched by Rust.
set -eu

if [ "${BPFMAN_KERNEL_TEST_LOCK_HELD:-}" = 1 ]; then
    exec "$@"
fi

lock=/tmp/bpfman-kernel-test-gate.lock
# Create once without truncating or replacing the inode. Open read-only so
# runs under different user IDs can acquire the same Linux flock.
if [ ! -e "$lock" ]; then
    (umask 022; set -C; : > "$lock") 2>/dev/null || [ -e "$lock" ]
fi
exec 9< "$lock"
if flock -n -E 75 9; then
    export BPFMAN_KERNEL_TEST_LOCK_HELD=1
    # Keep the descriptor in this shell. Test children may outlive their parent.
    "$@" 9<&-
else
    status=$?
    if [ "$status" -eq 75 ]; then
        printf 'Another bpfman kernel test run holds %s. Retry when it finishes.\n' "$lock" >&2
    fi
    exit "$status"
fi
