#!/bin/sh
# Fixture selection, exit status and owned cleanup without sudo or kernel state.
set -eu
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT HUP INT TERM
mkdir "$root/tools" "$root/fixtures" "$root/original"
REAL_MKTEMP=$(command -v mktemp)
export REAL_MKTEMP
export FAKE_FIXTURE_PARENT="$root/fixtures"
export FAKE_FIXTURE_LOG="$root/calls"
export TMPDIR="$root/original"
export BPFMAN_FAKE_KERNEL_TMPFS=1
cat > "$root/tools/stat" <<'MOCK'
#!/bin/sh
[ "$*" = '-f -c %T /dev/shm' ] || exit 1
printf '%s\n' "${FAKE_SHM_TYPE:-tmpfs}"
MOCK
cat > "$root/tools/mktemp" <<'MOCK'
#!/bin/sh
case "$2" in
    /dev/shm/*)
        [ "${FAKE_SHM_FAILURE:-0}" = 0 ] || exit 1
        exec "$REAL_MKTEMP" -d "$FAKE_FIXTURE_PARENT/ram.XXXXXXXX"
        ;;
    *) exec "$REAL_MKTEMP" -d "$FAKE_FIXTURE_PARENT/disk.XXXXXXXX" ;;
esac
MOCK
cat > "$root/kernel_lifecycle-fake" <<'MOCK'
#!/bin/sh
set -eu
trap 'exit 0' TERM
case "${EXPECTED_FIXTURE:-ram}" in
    original) [ "$TMPDIR" = "${FAKE_FIXTURE_PARENT%/fixtures}/original" ] ;;
    *) case "$TMPDIR" in "$FAKE_FIXTURE_PARENT/$EXPECTED_FIXTURE."*) ;; *) exit 90 ;; esac ;;
esac
printf '%s\n' "$TMPDIR" "$@" > "$FAKE_FIXTURE_LOG"
if [ "${FAKE_RESIDUE:-0}" = 1 ]; then touch "$TMPDIR/residue"; fi
if [ "${FAKE_WAIT:-0}" = 1 ]; then
    while :; do sleep 0.05; done
fi
exit "${FAKE_EXIT:-0}"
MOCK
chmod +x "$root/tools/stat" "$root/tools/mktemp" "$root/kernel_lifecycle-fake"
export PATH="$root/tools:$PATH"
runner="${0%/*}/test-runner.sh"

check_clean() {
    [ ! -d "$(sed -n '1p' "$FAKE_FIXTURE_LOG")" ]
    [ "$(tail -n 1 "$root/output")" = "$(printf 'fake-kernel-cleanup\tpassed')" ]
}
EXPECTED_FIXTURE=ram; export EXPECTED_FIXTURE
sh "$runner" "$root/kernel_lifecycle-fake" --exact 'case with spaces' > "$root/output" 2>&1
check_clean
printf '%s\n' --exact 'case with spaces' > "$root/expected"
sed '1d' "$FAKE_FIXTURE_LOG" > "$root/args"
cmp "$root/expected" "$root/args"

# Missing tmpfs, allocation failure and an explicit disk override all fall back.
EXPECTED_FIXTURE=disk; export EXPECTED_FIXTURE
FAKE_SHM_TYPE=other sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1
check_clean
FAKE_SHM_FAILURE=1 sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1
check_clean
BPFMAN_FAKE_KERNEL_TMPFS=0 sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1
check_clean

EXPECTED_FIXTURE=ram; export EXPECTED_FIXTURE
status=0
FAKE_EXIT=7 sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1 || status=$?
[ "$status" -eq 7 ]
check_clean
if FAKE_RESIDUE=1 sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1; then exit 1; fi
[ -f "$(sed -n '1p' "$FAKE_FIXTURE_LOG")/residue" ]
[ "$(tail -n 1 "$root/output")" = "$(printf 'fake-kernel-cleanup\tfailed\t%s' "$(sed -n '1p' "$FAKE_FIXTURE_LOG")")" ]
rm -rf "$(sed -n '1p' "$FAKE_FIXTURE_LOG")"

# Forward termination, wait for the child, then remove the empty scope.
rm "$FAKE_FIXTURE_LOG"
FAKE_WAIT=1 sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1 &
runner_pid=$!
attempt=0
while [ ! -s "$FAKE_FIXTURE_LOG" ]; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 100 ]
    sleep 0.01
done
kill -TERM "$runner_pid"
status=0
wait "$runner_pid" || status=$?
[ "$status" -eq 143 ]
check_clean

# Other Cargo targets retain their incoming TMPDIR and arguments.
cp "$root/kernel_lifecycle-fake" "$root/ordinary-test"
EXPECTED_FIXTURE=original sh "$runner" "$root/ordinary-test" > "$root/output" 2>&1
[ -d "$root/original" ]
[ ! -s "$root/output" ]
rm "$FAKE_FIXTURE_LOG"
if BPFMAN_FAKE_KERNEL_TMPFS=invalid sh "$runner" "$root/kernel_lifecycle-fake" > "$root/output" 2>&1; then exit 1; fi
[ ! -f "$FAKE_FIXTURE_LOG" ]
printf 'fake kernel fixture runner contracts passed\n'
