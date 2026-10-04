#!/bin/sh
# Cargo builds as the invoking user. Only the kernel suite needs privileges.
set -eu

case "${1##*/}" in
    kernel-*)
        exec sudo -n \
            --preserve-env=BPFMAN_GO_BIN,BPFMAN_DSL_TEST_BIN,BPFMAN_SHELL_BIN_DIR \
            unshare --mount --propagation private -- "$@" --test-threads=1
        ;;
    *)
        exec "$@"
        ;;
esac
