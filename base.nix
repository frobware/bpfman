{ pkgs ? import <nixpkgs> { } }:

# Minimal runtime base image for bpfman development
#
# This creates a lightweight base image containing only the essential
# runtime dependencies needed for bpfman binaries to run.
#
# Usage:
#   nix-build base.nix  # Build the base image
#
# The resulting base image can then be used in Containerfile.nix
# with fast COPY operations for development workflows.

let
  # Essential runtime dependencies
  runtime-env = pkgs.buildEnv {
    name = "bpfman-runtime-base";
    paths = [
      pkgs.cacert          # CA certificates for HTTPS
      pkgs.glibc           # C library (required for most binaries)
      pkgs.gcc-unwrapped.lib  # libgcc (required for Rust binaries)
      pkgs.busybox         # Basic shell utilities including /bin/sh
      pkgs.coreutils       # Core utilities (chmod, etc.)
    ];
    pathsToLink = [ "/etc" "/lib" "/lib64" "/bin" ];
  };

in
pkgs.dockerTools.buildImage {
  name = "bpfman-base";
  tag = "dev";

  # Copy runtime environment to image root
  copyToRoot = runtime-env;

  config = {
    # Set up environment for running bpfman binaries
    Env = [
      "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/usr/local/bin:/bin"
    ];
    
    # Use non-root user for security
    User = "65532:65532";
    
    # Working directory
    WorkingDir = "/";
    
    # Labels for identification
    Labels = {
      "org.opencontainers.image.title" = "bpfman-base";
      "org.opencontainers.image.description" = "Minimal runtime base for bpfman development";
      "built-with" = "nix";
    };
  };
}