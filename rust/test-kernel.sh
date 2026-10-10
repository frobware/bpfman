#!/bin/sh
# Complete suites discover every libtest case before running backend stages.
# Focused Cargo invocations continue to use the ordinary runner directly.
set -eu

# Linux uptime is monotonic and avoids clock-adjustment errors in stage totals.
elapsed() {
    read -r ended ignored < /proc/uptime
    awk -v begin="$1" -v end="$ended" 'BEGIN { printf "%.2f", end - begin }'
}
read -r suite_started ignored < /proc/uptime

binary=${1:?Kernel test binary required}
shift
[ "$#" -eq 0 ] || { echo 'kernel suites do not accept additional filters' >&2; exit 1; }
threads=${BPFMAN_KERNEL_TEST_THREADS:-4}
case "$threads" in 1|2|4) ;; *) echo 'kernel test threads must be 1, 2 or 4' >&2; exit 1 ;; esac
export BPFMAN_KERNEL_TEST_THREADS=$threads
mode=${BPFMAN_KERNEL_SUITE:-all}
case "$mode" in all|contracts) ;; *) echo 'unknown kernel suite' >&2; exit 1 ;; esac

listing=$(mktemp)
trap 'rm -f "$listing"' EXIT HUP INT TERM
"$binary" --list --format terse > "$listing"
discovered=0
while IFS= read -r line; do
    case "$line" in
        json_*': test'|sqlite_*': test'|go_*': test') discovered=$((discovered + 1)) ;;
        *) echo "unclassified kernel test: $line" >&2; exit 1 ;;
    esac
done < "$listing"
[ "$discovered" -gt 0 ] || { echo 'empty kernel test suite' >&2; exit 1; }

selected=0
for stage in corpus json-global json sqlite-global sqlite go; do
    if [ "$stage" = corpus ] && [ "$mode" = contracts ]; then continue; fi
    set --
    while IFS= read -r line; do
        name=${line%': test'}
        case "$stage:$name" in
            corpus:*_script_corpus) set -- "$@" "$name" ;;
            *:*_script_corpus) ;;
            json-global:json_xdp_load_lifecycle|sqlite-global:sqlite_xdp_load_lifecycle) set -- "$@" "$name" ;;
            *:json_xdp_load_lifecycle|*:sqlite_xdp_load_lifecycle) ;;
            json:json_*|sqlite:sqlite_*|go:go_*) set -- "$@" "$name" ;;
        esac
    done < "$listing"
    [ "$#" -gt 0 ] || continue
    selected=$((selected + $#))
    stage_threads=$threads
    case "$stage" in corpus|go|*-global) stage_threads=1 ;; esac
    printf 'kernel-stage\t%s\tthreads=%s\ttests=%s\n' "$stage" "$stage_threads" "$#"
    # Exact filters avoid accidental substring overlap, and each stage is joined
    # before the next begins. The Go script runners require exclusive suite locks.
    read -r stage_started ignored < /proc/uptime
    "$binary" --exact --test-threads="$stage_threads" "$@"
    printf 'kernel-stage-timing\t%s\t%s\n' "$stage" "$(elapsed "$stage_started")"
done
if [ "$mode" = all ] && [ "$selected" -ne "$discovered" ]; then
    echo "kernel suite selected $selected of $discovered tests" >&2
    exit 1
fi
printf 'kernel-suite-counts\tdiscovered=%s\tselected=%s\n' "$discovered" "$selected"
printf 'kernel-suite-timing\t%s\t%s\n' "$mode" "$(elapsed "$suite_started")"
