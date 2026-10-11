#!/bin/sh
# Scope memory-backed fixtures to the fake kernel, keeping real stores and fsync.
set -eu

: "${1:?Test binary required}"
storage=default
fixture_root=
case "${BPFMAN_FAKE_KERNEL_TMPFS:-1}" in
    1)
        if [ "$(stat -f -c %T /dev/shm 2>/dev/null || true)" = tmpfs ]; then
            if fixture_root=$(mktemp -d /dev/shm/bpfman-kernel-fake.XXXXXXXX 2>/dev/null); then
                storage=tmpfs
            else
                fixture_root=
            fi
        fi
        ;;
    0) ;;
    *)
        printf 'BPFMAN_FAKE_KERNEL_TMPFS must be 0 or 1\n' >&2
        exit 1
        ;;
esac
if [ -z "$fixture_root" ]; then
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/bpfman-kernel-fake.XXXXXXXX")
fi

child=
# Invoked indirectly by the EXIT trap.
# shellcheck disable=SC2329
finish() {
    status=$?
    trap - EXIT
    if rmdir "$fixture_root"; then
        printf 'fake-kernel-cleanup\tpassed\n' >&2
    else
        printf 'fake-kernel-cleanup\tfailed\t%s\n' "$fixture_root" >&2
        if [ "$status" -eq 0 ]; then status=1; fi
    fi
    exit "$status"
}
# Invoked indirectly by the signal traps.
# shellcheck disable=SC2329
stop() {
    trap '' HUP INT TERM
    if [ -n "$child" ]; then
        kill -"$1" "$child" 2>/dev/null || true
        wait "$child" || true
    fi
    exit "$2"
}
trap finish EXIT
trap 'stop HUP 129' HUP
trap 'stop INT 130' INT
trap 'stop TERM 143' TERM

printf 'fake-kernel-fixtures\t%s\t%s\n' "$storage" "$fixture_root" >&2
TMPDIR="$fixture_root" "$@" <&0 &
child=$!
if wait "$child"; then status=0; else status=$?; fi
child=
exit "$status"
