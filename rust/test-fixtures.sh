#!/bin/sh
# Owned fixture scopes for kernel tests, keeping real stores and synchronization.
set -eu

scope=${1:?Fixture scope required}
selection=${2:?Tmpfs selection required}
shift 2
case "$scope" in fake-kernel|real-kernel) ;; *) echo 'unknown fixture scope' >&2; exit 1 ;; esac
: "${1:?Test command required}"
storage=default
fixture_root=
case "$selection" in
    1)
        if [ "$(stat -f -c %T /dev/shm 2>/dev/null || true)" = tmpfs ]; then
            if fixture_root=$(mktemp -d "/dev/shm/bpfman-$scope.XXXXXXXX" 2>/dev/null); then
                storage=tmpfs
            else
                fixture_root=
            fi
        fi
        ;;
    0) ;;
    *)
        printf '%s tmpfs selection must be 0 or 1\n' "$scope" >&2
        exit 1
        ;;
esac
if [ -z "$fixture_root" ]; then
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/bpfman-$scope.XXXXXXXX")
fi

child=
# Invoked indirectly by the EXIT trap.
# shellcheck disable=SC2329
finish() {
    status=$?
    trap - EXIT
    if rmdir "$fixture_root"; then
        printf '%s-cleanup\tpassed\n' "$scope" >&2
    else
        printf '%s-cleanup\tfailed\t%s\n' "$scope" "$fixture_root" >&2
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

printf '%s-fixtures\t%s\t%s\n' "$scope" "$storage" "$fixture_root" >&2
TMPDIR="$fixture_root" "$@" <&0 &
child=$!
if wait "$child"; then status=0; else status=$?; fi
child=
exit "$status"
