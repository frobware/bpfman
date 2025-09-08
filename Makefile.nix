# Simple Makefile for streamlined bpfman development
# One-time setup: make base
# Fast iterations: make (default)

# Image building tool (docker / podman) - follows bpfman-operator convention
OCI_BIN_PATH := $(shell which docker 2>/dev/null || which podman)
OCI_BIN ?= $(shell basename ${OCI_BIN_PATH})

# Configuration - follows bpfman-operator patterns
IMAGE_TAG ?= latest
BPFMAN_IMG ?= docker.io/library/bpfman:$(IMAGE_TAG)
BASE_IMAGE := bpfman-base

.PHONY: all base dev test run run-bpfman clean help

# Default target - fast development build
all: dev

# One-time setup: build base image with dynamic dependencies
base:
	@echo "Building base image with dynamic dependencies..."
	@./build-nix-base.sh

# Fast development cycle: cargo build + container build
dev:
	cargo build
	$(OCI_BIN) build -f Containerfile.nix -t $(BPFMAN_IMG) .

# Test container functionality
test:
	@echo "Testing container..."
	@$(OCI_BIN) run --rm $(BPFMAN_IMG) --version
	@$(OCI_BIN) run --rm --entrypoint=/usr/local/bin/bpfman $(BPFMAN_IMG) --version

# Run container with default entrypoint (bpfman-rpc)
run:
	@$(OCI_BIN) run --rm $(BPFMAN_IMG) $(ARGS)

# Run bpfman binary in container
run-bpfman:
	@$(OCI_BIN) run --rm --entrypoint=/usr/local/bin/bpfman $(BPFMAN_IMG) $(ARGS)

# Clean build artifacts
clean:
	@rm -rf ./target/debug/bpfman* || true
	@$(OCI_BIN) rmi $(BPFMAN_IMG) 2>/dev/null || true

# Clean everything including base image and temp files
clean-all: clean
	@$(OCI_BIN) rmi $(BASE_IMAGE):$(IMAGE_TAG) 2>/dev/null || true
	@rm -rf ./nix-deps || true
	@rm -f Containerfile.base || true
	@cargo clean

# Help
help:
	@echo "Simple bpfman development Makefile"
	@echo ""
	@echo "Setup (run once):"
	@echo "  base         - Build base image with dynamic dependencies"
	@echo ""
	@echo "Development:"
	@echo "  all          - Fast build: cargo + container (default)"
	@echo "  dev          - Same as 'all'"
	@echo ""
	@echo "Testing:"
	@echo "  test         - Test container functionality"
	@echo "  run          - Run container (bpfman-rpc) with ARGS"
	@echo "  run-bpfman   - Run bpfman binary with ARGS"
	@echo ""
	@echo "Utilities:"
	@echo "  clean        - Clean build artifacts"
	@echo "  clean-all    - Deep clean including base image"
	@echo "  help         - Show this help"
	@echo ""
	@echo "Workflow:"
	@echo "  1. make base    # One-time setup"
	@echo "  2. make         # Fast development iterations"
	@echo ""
	@echo "Examples:"
	@echo "  make base                   # Initial setup"
	@echo "  make                        # Fast build"
	@echo "  make run ARGS='--help'      # Run bpfman-rpc --help"
	@echo "  make run-bpfman ARGS='--version'  # Run bpfman --version"
