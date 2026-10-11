#!/bin/sh
# Cargo builds as the invoking user. Only the kernel suite needs privileges.
set -eu

case "${1##*/}" in
    kernel_lifecycle-*)
        exec sh "${0%/*}/test-kernel-fake.sh" "$@"
        ;;
    kernel-*)
        if [ -n "${BPFMAN_KERNEL_SUITE:-}" ]; then
            exec sudo -n \
                --preserve-env=BPFMAN_GO_BIN,BPFMAN_DSL_TEST_BIN,BPFMAN_SHELL_BIN_DIR,BPFMAN_KERNEL_TIMINGS,BPFMAN_E2E_SCRIPT_TIMELINE,RUST_LOG,BPFMAN_KERNEL_SUITE,BPFMAN_KERNEL_TEST_THREADS,PARALLEL \
                unshare --mount --propagation private -- sh "${0%/*}/test-kernel.sh" "$@"
        fi
        exec sudo -n \
            --preserve-env=BPFMAN_GO_BIN,BPFMAN_DSL_TEST_BIN,BPFMAN_SHELL_BIN_DIR,BPFMAN_KERNEL_TIMINGS,BPFMAN_E2E_SCRIPT_TIMELINE,RUST_LOG,PARALLEL \
            unshare --mount --propagation private -- "$@" --test-threads=1
        ;;
    *)
        exec "$@"
        ;;
esac
