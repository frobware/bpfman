{
  description = "A Nix flake for bpfman.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs = { self, nixpkgs, rust-overlay }: let
    forAllSystems = function: nixpkgs.lib.genAttrs [
      "aarch64-linux"
      "x86_64-linux"
    ] (system: function system);
  in {
    devShells = forAllSystems (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };

      bpf2go = pkgs.buildGoModule rec {
        pname = "bpf2go";
        version = "0.14.0";
        src = pkgs.fetchFromGitHub {
          owner = "cilium";
          repo = "ebpf";
          rev = "v${version}";
          sha256 = "sha256-GJgSSyN5grQInOuCBTQVaZZ0CaIRMB1wmbbI4jaot0Q=";
        };
        doCheck = false;
        subPackages = [ "cmd/bpf2go" ];
        vendorHash = "sha256-8QePzWyX8egfg1qO1NjdcMiJjPdO5jXetWerphGy3H8=";
      };

      # Rust choices:
      #
      # rustEnv = pkgs.rust-bin.nightly.latest.default;
      # rustEnv = pkgs.rust-bin.stable.latest.default;
      rustEnv = pkgs.rust-bin.stable.latest.default.override {
        extensions = [ "rust-src" ];
      };
    in {
      default = pkgs.mkShell {
        buildInputs = [
          bpf2go
          rustEnv

          pkgs.bpftool
          pkgs.cargo-edit
          pkgs.cargo-udeps
          pkgs.clang
          pkgs.go_1_22
          pkgs.kind
          pkgs.libbpf
          pkgs.libelf
          pkgs.mold
          pkgs.openssl_3
          pkgs.pkg-config
          pkgs.pkgsi686Linux.glibc
          pkgs.protobuf3_23
          pkgs.protoc-gen-go
          pkgs.protoc-gen-go-grpc
          pkgs.rust-analyzer
          pkgs.taplo
          pkgs.zlib
        ];

        shellHook = ''
          export CLANG=${pkgs.clang}/bin/clang
          export RUST_SRC_PATH="${rustEnv}/lib/rustlib/src/rust/library"
        '';
      };
    });
  };
}
