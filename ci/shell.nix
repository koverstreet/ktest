# CI worker build environment.
#
# Provides all dependencies for kernel builds (including Rust) and test
# execution. Replaces the build-related entries in configuration.nix so
# CI machines can run a minimal NixOS config.
#
# Usage:
#   ci/ci-worker [test-git-branch.sh args...]    # preferred
#   nix-shell ci/shell.nix --run '...'           # manual

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    # kernel build essentials
    gnumake gcc clang binutils
    flex bison bc
    pkg-config pahole
    elfutils ncurses openssl zlib

    # Rust for kernel
    rustc cargo rust-bindgen rust-analyzer
    llvmPackages.libclang

    # test infrastructure
    qemu
    brotli lcov
    python3
    gdb socat
    perl
  ];

  RUST_LIB_SRC = "${pkgs.rustPlatform.rustLibSrc}";
}
