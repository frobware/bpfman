#!/bin/bash
# Build base image with dynamically determined dependencies
set -euo pipefail

: "${OCI_BIN:=docker}"

log() {
    echo "[build-base] $1"
}

log "Cleaning old dependencies..."
rm -rf ./nix-deps

log "Analyzing binary dependencies..."

# Get all unique /nix/store paths from all binaries using ldd + interpreter
DEPS=$(for binary in ./target/debug/bpfman*; do
           if [ -f "$binary" ]; then
               # Get shared library dependencies
               ldd "$binary" 2>/dev/null | grep "/nix/store" | awk '{print $3}' | grep "^/nix/store" || true
               # Get dynamic linker interpreter
               patchelf --print-interpreter "$binary" 2>/dev/null | grep "^/nix/store" || true
           fi
       done | sort -u)

log "Found dependencies:"
echo "$DEPS"

log "Copying dependencies to ./nix-deps..."
rm -rf ./nix-deps
mkdir -p ./nix-deps
for dep in $DEPS; do
    dep_dir="./nix-deps$(dirname "$dep")"
    mkdir -p "$dep_dir"
    cp "$dep" "$dep_dir/"
    log "  Copied $dep"
done

log "Generating Containerfile.base..."

cat > Containerfile.base << 'EOF'
# Dynamically generated base image with bpfman runtime dependencies
FROM scratch

EOF

# Add COPY commands for each dependency from nix-deps
for dep in $DEPS; do
    echo "COPY ./nix-deps$dep $dep" >> Containerfile.base
done

cat >> Containerfile.base << 'EOF'

# Labels
LABEL org.opencontainers.image.title="bpfman-base"
LABEL org.opencontainers.image.description="Base image with bpfman runtime dependencies"
LABEL build-method="dynamic-deps"
EOF

log "Building base image..."

$OCI_BIN build -f Containerfile.base -t bpfman-base:latest .

log "Base image built successfully"
