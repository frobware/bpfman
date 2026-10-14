#!/bin/sh
# Fake-kernel suites use the same owned fixture scope as real-kernel suites.
set -eu
exec sh "${0%/*}/test-fixtures.sh" fake-kernel "${BPFMAN_FAKE_KERNEL_TMPFS:-1}" "$@"
