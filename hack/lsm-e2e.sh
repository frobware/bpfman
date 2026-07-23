#!/usr/bin/env bash
#
# Build and run the lsm e2e on a Fedora system whose kernel has "bpf" in
# the active LSM list (so BPF_PROG_TYPE_LSM programs can attach).
# Idempotent: installs build deps, builds, loads the e2e kmod, then runs
# the lsm gRPC lifecycle sub-test and the lsm .bpfman scripts.
#
# Meant to be handed to hack/fedora-vm.sh (which supplies exactly such a
# Fedora guest); --provision caches a disk with the deps pre-installed:
#
#   hack/fedora-vm.sh --provision hack/install-fedora-deps.sh --run hack/lsm-e2e.sh
#
# but it also runs directly on any Fedora box with bpf-LSM active
# (stock Fedora kernels ship it on by default).

set -euo pipefail

cd "$(dirname "$0")/.."

# Persist the Go build cache on the repository (the virtiofs share under
# hack/fedora-vm.sh) so a rerun in a fresh VM compiles incrementally
# instead of from cold. Harmless on a normal filesystem.
export GOCACHE="$PWD/.gocache"

if ! grep -qw bpf /sys/kernel/security/lsm; then
    echo "error: bpf is not in the active LSM list ($(cat /sys/kernel/security/lsm))." >&2
    echo "       BPF_PROG_TYPE_LSM programs cannot attach here." >&2
    echo "       Use a kernel with bpf-LSM (stock Fedora) or boot with lsm=...,bpf." >&2
    exit 1
fi

hack/install-fedora-deps.sh

# Build binaries, the e2e test binaries, and the e2e kmod up front.
make bpfman-compile
make build-e2e-grpc build-e2e-scripts
make e2e-kmod-reload

# The lsm gRPC lifecycle sub-test (TestParallel_GRPC gates on the kmod
# loaded above) and the two lsm .bpfman scripts.
make test-e2e-grpc    TEST='TestParallel_GRPC/lsm'              STRESS_COUNT=1
make test-e2e-scripts TEST='TestBPFManScripts/scripts/TestLsm_' STRESS_COUNT=1
