{ pkgs ? import <nixpkgs> { } }:

# Crane-based Nix build for bpfman with optimized dependency caching
#
# This build uses crane to separate dependency compilation from source compilation,
# providing significant performance improvements for development workflows.
#
# Outputs:
#   - bpfman: Binary package with all executables (default)
#   - oci-image: Layered container image with optimized caching
#   - cargoArtifacts: Cached dependency build artifacts (for internal use)
#
# Usage:
#   nix-build crane.nix                    # Build all outputs (default: bpfman)
#   nix-build crane.nix -A bpfman         # Build only the binary package
#   nix-build crane.nix -A oci-image      # Build only the layered container image
#   nix-build crane.nix -A cargoArtifacts # Build only cached dependencies
#
# The build uses debug profile by default for faster compilation during development.
# Dependencies are cached separately, so subsequent builds only recompile changed source code.
#
# Container Optimization:
# - Uses buildLayeredImage for optimal Docker layer caching
# - Runtime environment (CA certs) in separate layer from binaries
# - Pure Nix approach: no Ubuntu base, minimal footprint
# - Only binary layer rebuilds when source code changes

let
  # Use crane from the added channel
  crane = import <crane> { inherit pkgs; };
  craneLib = crane.craneLib;

  # Read Cargo.toml for metadata
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

  # Common build arguments
  commonArgs = {
    src = craneLib.cleanCargoSource ./.;
    pname = "bpfman";
    version = cargoToml.workspace.package.version;

    nativeBuildInputs = with pkgs; [
      pkg-config
      cmake
      mold  # Fast linker for improved build performance
      clang # Required for mold integration
    ];

    buildInputs = with pkgs; [
      openssl
      libclang
    ];

    # Environment for bindgen
    LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

    # Use debug profile instead of default release
    CARGO_PROFILE = "dev";

    # Don't strip debug symbols for debug builds
    dontStrip = true;
    
    # Skip all fixups for faster development builds
    dontFixup = true;
    
    # Disable both reference stripping hooks for development builds
    doNotRemoveReferencesToVendorDir = "1";
    doNotRemoveReferencesToRustToolchain = "1";
    
    
    # Use mold linker for faster linking through clang
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "${pkgs.clang}/bin/clang";
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=${pkgs.mold}/bin/mold";
  };

  # Build dependencies ONLY (for maximum caching)
  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    # Skip tests in dependency build for speed
    doCheck = false;
  });

  # Build the actual binaries (reusing cached deps)
  bpfman = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;

    # Skip tests for faster builds
    doCheck = false;

    # Build only what we need
    cargoBuildFlags = [ "--bin" "bpfman" "--bin" "bpfman-ns" "--bin" "bpfman-rpc" ];
    

    meta = with pkgs.lib; {
      description = "eBPF manager";
      homepage = "https://github.com/bpfman/bpfman";
      license = licenses.asl20;
    };
  });

  # Runtime essentials layer (cached separately from binaries)
  runtime-env = pkgs.buildEnv {
    name = "bpfman-runtime";
    paths = [
      pkgs.cacert  # CA certificates for HTTPS
    ];
    pathsToLink = [ "/etc" ];
  };

  # Simple fast-building OCI image using pure Nix approach
  oci-image = pkgs.dockerTools.buildImage {
    name = "bpfman";
    tag = "crane";

    # Simple single-layer approach for fast development builds
    copyToRoot = pkgs.buildEnv {
      name = "image-root";
      paths = [
        runtime-env  # CA certificates
        bpfman       # All binaries
      ];
    };

    config = {
      Entrypoint = [ "${bpfman}/bin/bpfman-rpc" ];
      Cmd = [ "--timeout=0" ];
      User = "65532:65532";
      Env = [
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      ];
      Labels = {
        "org.opencontainers.image.title" = "bpfman";
        "org.opencontainers.image.version" = cargoToml.workspace.package.version;
        "built-with" = "crane + layered nix";
      };
    };
  };

in
{
  inherit bpfman oci-image cargoArtifacts;
  default = bpfman;
}
