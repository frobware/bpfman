#!/bin/sh
# Discover every case and run exact cases in independent, bounded processes.
# Focused Cargo invocations continue to use the ordinary runner directly.
set -eu

elapsed() {
    read -r ended _ignored < /proc/uptime
    awk -v begin="$1" -v end="$ended" 'BEGIN { printf "%.2f", end - begin }'
}
read -r suite_started _ignored < /proc/uptime

binary=${1:?Kernel test binary required}
shift
[ "$#" -eq 0 ] || { echo 'kernel suites do not accept additional filters' >&2; exit 1; }
# Keep the existing environment knob: it now bounds processes, rather than
# libtest threads. Nested forwarding workers use this same limit as before.
workers=${BPFMAN_KERNEL_TEST_THREADS:-8}
case "$workers" in 1|2|4|8) ;; *) echo 'kernel test workers must be 1, 2, 4 or 8' >&2; exit 1 ;; esac
export BPFMAN_KERNEL_TEST_THREADS="$workers"
mode=${BPFMAN_KERNEL_SUITE:-all}
case "$mode" in all|contracts) ;; *) echo 'unknown kernel suite' >&2; exit 1 ;; esac

scratch=$(mktemp -d)
pids=
# Workers are direct children in separate process groups. Always join all of
# them, including after a failed case or a signal, before removing runner logs.
# shellcheck disable=SC2329
finish() {
    status=$?
    trap - EXIT
    trap '' HUP INT TERM
    if [ -n "$pids" ]; then
        for pid in $pids; do
            kill -s TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
        done
        for pid in $pids; do wait "$pid" || true; done
    fi
    rm -rf "$scratch"
    exit "$status"
}
trap finish EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
listing="$scratch/list"
"$binary" --list --format terse > "$listing"
discovered=0
while IFS= read -r line; do
    case "$line" in
        sqlite_*': test'|go_*': test') ;;
        *) echo "unclassified kernel test: $line" >&2; exit 1 ;;
    esac
    name=${line%': test'}
    case "$name" in *[!a-zA-Z0-9_:]*) echo "invalid kernel test name: $name" >&2; exit 1 ;; esac
    discovered=$((discovered + 1))
done < "$listing"
[ "$discovered" -gt 0 ] || { echo 'empty kernel test suite' >&2; exit 1; }
if [ -n "$(sort "$listing" | uniq -d)" ]; then
    echo 'duplicate kernel test name' >&2
    exit 1
fi

selected=0
for stage in corpus sqlite-global sqlite go; do
    if [ "$stage" = corpus ] && [ "$mode" = contracts ]; then continue; fi
    set --
    while IFS= read -r line; do
        name=${line%': test'}
        case "$stage:$name" in
            corpus:*_script_corpus) set -- "$@" "$name" ;;
            *:*_script_corpus) ;;
            # This case enumerates all programs, temporarily opening foreign
            # program FDs even across process boundaries. Keep it serial.
            sqlite-global:sqlite_xdp_load_lifecycle) set -- "$@" "$name" ;;
            *:sqlite_xdp_load_lifecycle) ;;
            sqlite:sqlite_*|go:go_*) set -- "$@" "$name" ;;
        esac
    done < "$listing"
    [ "$#" -gt 0 ] || continue
    selected=$((selected + $#))
    stage_workers=$workers
    case "$stage" in corpus|go|*-global) stage_workers=1 ;; esac
    if [ "$stage_workers" -gt "$#" ]; then stage_workers=$#; fi
    printf 'kernel-stage\t%s\tprocesses=%s\ttests=%s\n' "$stage" "$stage_workers" "$#"
    read -r stage_started _ignored < /proc/uptime

    # Round-robin queues bound concurrency without a controller that ever owns
    # BPF resources. Workers wait for one case to exit before starting the next.
    for worker in $(seq 1 "$stage_workers"); do : > "$scratch/queue-$worker"; done
    worker=1
    for name do
        printf '%s\n' "$name" >> "$scratch/queue-$worker"
        worker=$((worker % stage_workers + 1))
    done
    for worker in $(seq 1 "$stage_workers"); do
        setsid sh "${0%/*}/test-kernel-worker.sh" "$binary" "$scratch" "$scratch/queue-$worker" &
        pids="${pids:+$pids }$!"
    done
    failed=0
    for pid in $pids; do
        if ! wait "$pid"; then failed=1; fi
        pids=${pids#"$pid"}
        pids=${pids# }
    done
    pids=
    for name do
        if [ -f "$scratch/$name.result" ]; then
            read -r status pid < "$scratch/$name.result"
            printf 'kernel-case\t%s\tpid=%s\tstatus=%s\n' "$name" "$pid" "$status"
            if [ "$status" -ne 0 ]; then failed=1; fi
        else
            printf 'kernel case produced no result: %s\n' "$name" >&2
            failed=1
        fi
        if [ -f "$scratch/$name.log" ]; then cat "$scratch/$name.log"; fi
    done
    [ "$failed" -eq 0 ] || exit 1
    printf 'kernel-stage-timing\t%s\t%s\n' "$stage" "$(elapsed "$stage_started")"
done
if [ "$mode" = all ] && [ "$selected" -ne "$discovered" ]; then
    echo "kernel suite selected $selected of $discovered tests" >&2
    exit 1
fi
printf 'kernel-suite-counts\tdiscovered=%s\tselected=%s\n' "$discovered" "$selected"
printf 'kernel-suite-timing\t%s\t%s\n' "$mode" "$(elapsed "$suite_started")"
