#!/bin/sh
# Fast scheduling contracts use a fake libtest binary; no sudo or kernel state.
set -eu
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT HUP INT TERM
export FAKE_KERNEL_LOG="$root/calls"
export FAKE_TEST_LIST="$root/list"
cat > "$root/kernel" <<'MOCK'
#!/bin/sh
if [ "$1" = --list ]; then cat "$FAKE_TEST_LIST"; exit; fi
printf '%s\n' "$*" >> "$FAKE_KERNEL_LOG"
case "$*" in *"${FAKE_FAILURE:-never-match}"*) exit 1 ;; esac
MOCK
chmod +x "$root/kernel"
cat > "$FAKE_TEST_LIST" <<'LIST'
json_script_corpus: test
sqlite_script_corpus: test
json_xdp_load_lifecycle: test
sqlite_xdp_load_lifecycle: test
json_case: test
json_case_longer: test
sqlite_case: test
go_case: test
LIST
runner="${0%/*}/test-kernel.sh"
for threads in 1 2 4; do
    : > "$FAKE_KERNEL_LOG"
    BPFMAN_KERNEL_TEST_THREADS=$threads BPFMAN_KERNEL_SUITE=all sh "$runner" "$root/kernel" > "$root/output"
    cat > "$root/expected" <<EXPECTED
--exact --test-threads=1 json_script_corpus sqlite_script_corpus
--exact --test-threads=1 json_xdp_load_lifecycle
--exact --test-threads=$threads json_case json_case_longer
--exact --test-threads=1 sqlite_xdp_load_lifecycle
--exact --test-threads=$threads sqlite_case
--exact --test-threads=1 go_case
EXPECTED
    cmp "$root/expected" "$FAKE_KERNEL_LOG"
done
: > "$FAKE_KERNEL_LOG"
BPFMAN_KERNEL_TEST_THREADS=2 BPFMAN_KERNEL_SUITE=contracts sh "$runner" "$root/kernel" > "$root/output"
sed '1d' "$root/expected" | sed 's/threads=4/threads=2/g' > "$root/contracts"
cmp "$root/contracts" "$FAKE_KERNEL_LOG"

# Invalid admission must have no effects; a failed stage stops later backends.
for invalid in 0 3 invalid; do
    : > "$FAKE_KERNEL_LOG"
    if BPFMAN_KERNEL_TEST_THREADS=$invalid sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
    [ ! -s "$FAKE_KERNEL_LOG" ]
done
: > "$FAKE_KERNEL_LOG"
if BPFMAN_KERNEL_SUITE=unknown sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
[ ! -s "$FAKE_KERNEL_LOG" ]
if sh "$runner" "$root/kernel" unexpected_filter > "$root/output" 2>&1; then exit 1; fi
[ ! -s "$FAKE_KERNEL_LOG" ]
FAKE_FAILURE=json_case; export FAKE_FAILURE
if BPFMAN_KERNEL_SUITE=all sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
[ "$(wc -l < "$FAKE_KERNEL_LOG")" -eq 3 ]
unset FAKE_FAILURE
: > "$FAKE_KERNEL_LOG"
printf 'unclassified: test\n' >> "$FAKE_TEST_LIST"
if sh "$runner" "$root/kernel" > "$root/output" 2>&1; then exit 1; fi
[ ! -s "$FAKE_KERNEL_LOG" ]
printf 'kernel runner scheduling contracts passed\n'
