#!/usr/bin/env bash

set -eu

host_arch=$(uname -m)

# Canonical architecture names.
# $ dpkg --print-foreign-architectures
declare -A canonical_arch_map
canonical_arch_map=(
    ["aarch64"]="arm64"
    ["amd64"]="x86_64"
    ["x86-64"]="x86_64"
    ["ppc64le"]="ppc64le"
    ["s390x"]="s390x"
)

# Rust target mappings.
declare -A rust_target_map
rust_target_map=(
    ["x86_64"]="x86_64-unknown-linux-gnu"
    ["arm64"]="aarch64-unknown-linux-gnu"
    ["ppc64le"]="powerpc64le-unknown-linux-gnu"
    ["s390x"]="s390x-unknown-linux-gnu"
)

# GCC toolchain mappings.
declare -A gcc_toolchain_map
gcc_toolchain_map=(
    ["x86_64"]="x86_64-linux-gnu"
    ["arm64"]="aarch64-linux-gnu"
    ["ppc64le"]="powerpc64le-linux-gnu"
    ["s390x"]="s390x-linux-gnu"
)

# GCC toolchain mappings.
declare -A gcc_pkg_toolchain_map
gcc_pkg_toolchain_map=(
    ["x86_64"]="gcc-x86-64-linux-gnu"
    ["arm64"]="gcc-aarch64-linux-gnu"
    ["ppc64le"]="gcc-powerpc64le-linux-gnu"
    ["s390x"]="gcc-s390x-linux-gnu"
)

# Debian architecture for package management (if different from
# canonical).
declare -A debian_arch_map
debian_arch_map=(
    ["x86_64"]="amd64"
    ["arm64"]="arm64"
    ["ppc64le"]="ppc64le"
    ["s390x"]="s390x"
)

# Function to canonicalise the architecture input.
canonicalise_arch() {
    local arch_input="$1"
    if [[ -n "${canonical_arch_map[$arch_input]+set}" ]]; then
        echo "${canonical_arch_map[$arch_input]}"
    else
        echo "Unsupported architecture: $arch_input" >&2
        exit 1
    fi
}

# Specify the target architecture.
target_arch_input=$1
if [ -z "$target_arch_input" ]; then
    echo "Usage: $0 <target-architecture>"
    echo "Supported architectures: amd64, arm64, ppc64le, s390x"
    exit 1
fi

# Canonicalise the architecture input.
target_arch=$(canonicalise_arch "$target_arch_input")
echo "$target_arch"

# Get Rust target based on the canonical architecture.
rust_target="${rust_target_map[$target_arch]}"

# Get GCC toolchain name based on the canonical architecture.
gcc_target="${gcc_toolchain_map[$target_arch]}"
cc="${gcc_target}-gcc"
linker="${gcc_target}-gcc"

echo "gcc_target=$gcc_target"
echo "cc=$cc"
echo "linker=$linker"

sysroot="/usr/${gcc_target}"
lib_dir="/usr/lib/${gcc_target}"

# Get Debian architecture name for package management.
debian_arch="${debian_arch_map[$target_arch]}"

# Set the appropriate version of libssl-dev depending on the
# architecture.
libssl_dev="libssl-dev"
if [ $host_arch != "$target_arch_input" ]; then
    libssl_dev="libssl-dev:${debian_arch}"
fi

echo "Setting up cross-compilation environment for $target_arch"

# Add foreign architectures only if we're cross-compiling.
if [ $host_arch != "$target_arch_input" ]; then
    ${SUDO:-} dpkg --add-architecture "$debian_arch"
fi

# Update package lists. (Required if an architecture has been added.)
${SUDO:-} apt-get update

# Install required dependencies for all targets.
${SUDO:-} apt-get install -y \
     clang \
     cmake \
     direnv \
     git \
     libelf-dev \
     libssl-dev \
     llvm \
     perl \
     pkg-config \
     protobuf-compiler

# Install cross-compilation toolchains and OpenSSL for the target architecture.
if [ "$host_arch" != "$target_arch_input" ]; then
    # Cross-compiling: host and target architectures are different
    ${SUDO:-} apt-get install -y $(gcc_pkg_toolchain_map ${target_arch}) "$libssl_dev"
else
    # Native compilation: host and target architectures are the same
    ${SUDO:-} apt-get install -y gcc "$libssl_dev"
fi

if [ $host_arch != "$target_arch_input" ]; then
    # Correct the paths for pkg-config and to find OpenSSL.
    export PKG_CONFIG_SYSROOT_DIR="/usr/${gcc_target}"
    export PKG_CONFIG_PATH="/usr/lib/${gcc_target}/pkgconfig:${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    echo "Configured pkg-config and OpenSSL for $target_arch"
fi

# Install Rust target for the specific architecture.
if ! rustup target list | grep -q "^$rust_target (installed)$"; then
    rustup target add "$rust_target"
fi

# Set RUSTFLAGS to use the correct cross-linker if cross-compiling.
if [ $host_arch != "$target_arch_input" ]; then
    export RUSTFLAGS="-C linker=$linker ${RUSTFLAGS:-}"
fi

export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=-crt-static"

# Build the project using cargo for the specified target.
echo "Building bpfman for $target_arch using Rust target $rust_target..."
OPENSSL_STATIC=0 CC=$cc cargo build ${CARGO_RELEASE:-} --target "$rust_target"

# Output the result
echo "Build complete for $target_arch"
