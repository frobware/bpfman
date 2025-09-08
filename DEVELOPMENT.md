# bpfman Development Build Guide

This guide covers the optimal hybrid build approach for bpfman development, combining the best of Nix runtime perfection with native Rust build performance.

## 🚀 Hybrid Development Workflow (Recommended)

The hybrid approach provides the fastest development iteration with optimal runtime dependencies:

### Quick Start

```bash
# 1. Build base image (one-time, ~3 seconds)
./build-dev.sh

# 2. For subsequent changes, just rebuild:
./build-dev.sh
```

### Manual Steps

```bash
# 1. Build minimal runtime base image (once)
nix-build base.nix -o result-base
podman load < result-base

# 2. Fast native cargo build (~0.25s incremental)
cargo build --profile dev

# 3. Fast Docker build with COPY (~2.7s)
podman build -f Containerfile.dev --ignorefile .dockerignore.dev -t bpfman:dev .

# Total incremental time: ~3 seconds
```

## 📊 Performance Comparison

| Approach | First Build | Incremental Build | Use Case |
|----------|-------------|-------------------|----------|
| **Hybrid** | ~75s | **~3s** | 🏆 **Development** |
| Pure Nix | ~90s | ~27s | CI/CD, Production |
| Docker Only | ~120s | ~45s | Traditional builds |

## 🏗️ Build Methods

### 1. Hybrid Build (Fastest Development)

**Files:**
- `base.nix` - Minimal Nix runtime base image
- `Containerfile.dev` - Fast COPY-based container build
- `.dockerignore.dev` - Allows debug binaries, ignores build artifacts
- `build-dev.sh` - Automated hybrid build script

**Workflow:**
1. Nix creates perfect minimal runtime base (once)
2. Native cargo uses local cache and mold linker (fast)
3. Docker COPYs binaries into base image (fast layers)

**Optimizations:**
- ✅ Nix base with CA certs, glibc, busybox, coreutils
- ✅ Native cargo with mold fast linker
- ✅ Docker layer caching for COPY operations
- ✅ Debug builds (no reference stripping)
- ✅ Separate .dockerignore.dev for development

### 2. Pure Nix Build

**Files:**
- `crane.nix` - Advanced crane-based Nix build

**Usage:**
```bash
# Binary only (~12s incremental)
nix-build crane.nix -A bpfman

# With container (~27s incremental)  
nix-build crane.nix
```

**Optimizations:**
- ✅ Crane dependency caching
- ✅ Reference stripping disabled (`doNotRemoveReferencesToVendorDir = "1"`)
- ✅ Debug profile builds (`CARGO_PROFILE = "dev"`)
- ✅ Mold linker integration
- ✅ Pure Nix container image

## 🔧 Development Tips

### Fast Iteration Loop

```bash
# Edit source code
vim bpfman/src/bin/cli/main.rs

# Quick rebuild and test (~3 seconds total)
cargo build --profile dev && \
podman build -f Containerfile.dev --ignorefile .dockerignore.dev -t bpfman:dev . && \
podman run --rm bpfman:dev --help
```

### Binary-Only Development

For even faster iteration when you don't need containers:

```bash
# Native build only (~0.25s incremental)
cargo build --profile dev

# Run directly  
./target/debug/bpfman --help
```

### Base Image Management

```bash
# Rebuild base image when runtime dependencies change
./build-dev.sh --rebuild-base

# Check base image
podman images bpfman-base:dev
```

## 🐛 Troubleshooting

### "No such file or directory" on COPY

The `.dockerignore` file is filtering out `target/` directory:

**Solution:** Use the development ignore file:
```bash
podman build -f Containerfile.dev --ignorefile .dockerignore.dev -t bpfman:dev .
```

### "executable file `/bin/sh` not found"

The base image is missing shell utilities:

**Solution:** Rebuild base image with updated `base.nix` that includes busybox and coreutils.

### Slow incremental builds

If incremental builds are slow, check:

1. **Cargo cache intact:** `ls target/debug/.fingerprint/`
2. **Using debug profile:** `cargo build --profile dev`
3. **Using development ignore file:** `--ignorefile .dockerignore.dev`

## 📁 File Reference

### Core Files

- **`base.nix`** - Minimal runtime base image with Nix
- **`Containerfile.dev`** - Development container build
- **`.dockerignore.dev`** - Development-optimized ignore rules  
- **`build-dev.sh`** - Automated hybrid build script
- **`crane.nix`** - Full Nix build with crane optimization

### Key Configuration

**`base.nix` runtime dependencies:**
```nix
paths = [
  pkgs.cacert          # CA certificates
  pkgs.glibc           # C library  
  pkgs.gcc-unwrapped.lib  # libgcc
  pkgs.busybox         # Shell utilities
  pkgs.coreutils       # chmod, etc.
];
```

**`crane.nix` optimizations:**
```nix
# Fast development builds
CARGO_PROFILE = "dev";
dontStrip = true;
dontFixup = true;
doNotRemoveReferencesToVendorDir = "1";
doNotRemoveReferencesToRustToolchain = "1";
```

## 🎯 Recommended Workflow

For **daily development**: Use hybrid approach with `./build-dev.sh`

For **testing Nix builds**: Use `nix-build crane.nix`  

For **production**: Use pure Nix builds for reproducibility

The hybrid approach gives you the best of both worlds: Nix's perfect runtime environment with native Rust development speed.