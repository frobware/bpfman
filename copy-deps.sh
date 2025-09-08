#!/bin/bash
# Extract and copy Nix store dependencies for bpfman binaries
set -euo pipefail

log() {
    echo "[copy-deps] $1"
}

# Create deps directory
mkdir -p ./nix-deps

log "Analyzing binary dependencies..."

# Get all unique /nix/store paths from all binaries (libraries + dynamic linker)
DEPS=$(for binary in ./target/debug/bpfman*; do
    if [ -f "$binary" ]; then
        # Get shared library dependencies
        ldd "$binary" 2>/dev/null | grep "/nix/store" | awk '{print $3}' | grep "^/nix/store" || true
        # Get dynamic linker path
        readelf -l "$binary" 2>/dev/null | grep "program interpreter" | sed 's/.*: //' | tr -d ']' | grep "^/nix/store" || true
    fi
done | sort -u)

log "Found dependencies:"
echo "$DEPS"

log "Copying dependencies to ./nix-deps/..."

# Copy each dependency maintaining directory structure
for dep in $DEPS; do
    if [ -f "$dep" ]; then
        # Create the directory structure
        dep_dir="./nix-deps$(dirname "$dep")"
        mkdir -p "$dep_dir"
        # Copy the file
        cp "$dep" "$dep_dir/"
        log "  Copied $dep"
    fi
done

log "Dependencies copied to ./nix-deps/"
log "Total size: $(du -sh ./nix-deps | cut -f1)"