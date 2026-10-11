#!/bin/sh
# All workspace tests except the privileged kernel integration binary. Cargo's
# auto-discovered CLI integration targets are selected by filename, not a list
# that could silently omit a newly added contract. rust-test runs kernel next.
set -eu

manifest=${1:?Rust manifest required}
shift
sh "${0%/*}/test-kernel-runner-test.sh"
sh "${0%/*}/test-kernel-fake-runner-test.sh"
cargo test --manifest-path "$manifest" --workspace --exclude bpfman --locked

# Preserve the Cargo runner supplied by Make while discovering every target.
for source in "${manifest%/Cargo.toml}"/crates/bpfman/tests/*.rs; do
    target=${source##*/}
    target=${target%.rs}
    case "$target" in
        kernel) continue ;;
    esac
    set -- "$@" --test "$target"
done

exec cargo test --manifest-path "$manifest" -p bpfman --bins --locked "$@"
