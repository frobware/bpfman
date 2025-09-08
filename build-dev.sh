#!/bin/bash
# Fast development build script for bpfman
# 
# This implements the optimal hybrid build workflow:
# 1. Nix builds minimal base image (once or when deps change)
# 2. Native cargo build (fast, uses local cache)
# 3. Docker build copies binaries (fast, single layer)

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() {
    echo -e "${BLUE}[build-dev]${NC} $1"
}

success() {
    echo -e "${GREEN}[build-dev]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[build-dev]${NC} $1"
}

error() {
    echo -e "${RED}[build-dev]${NC} $1"
}

# Check if base image exists or needs rebuild
BASE_IMAGE_PATH="./result-base"
REBUILD_BASE=false

if [[ ! -L "$BASE_IMAGE_PATH" ]] || [[ "$1" == "--rebuild-base" ]]; then
    REBUILD_BASE=true
fi

# Step 1: Build base image if needed
if [[ "$REBUILD_BASE" == "true" ]]; then
    log "Building minimal runtime base image..."
    time nix-build base.nix -o result-base
    
    # Load the base image into podman/docker
    BASE_IMAGE_TAR=$(readlink -f result-base)
    log "Loading base image: $BASE_IMAGE_TAR"
    podman load < "$BASE_IMAGE_TAR"
    success "Base image built and loaded!"
else
    log "Using existing base image (use --rebuild-base to rebuild)"
fi

# Step 2: Fast native cargo build
log "Building binaries with native cargo..."
time cargo build --profile dev

if [[ ! -f "./target/debug/bpfman" ]]; then
    error "Binary not found: ./target/debug/bpfman"
    error "Make sure cargo build succeeded"
    exit 1
fi

success "Binaries built in ./target/debug/"

# Step 3: Fast Docker build
log "Building development container image..."
time podman build -f Containerfile.dev -t bpfman:dev .

success "Development image ready: bpfman:dev"

# Show final size
log "Image details:"
podman images bpfman:dev --format "table {{.Repository}}:{{.Tag}}\t{{.Size}}\t{{.Created}}"

log "Usage:"
echo "  podman run --rm bpfman:dev --help"
echo "  podman run --rm -p 50051:50051 bpfman:dev"