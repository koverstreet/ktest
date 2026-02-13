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

let
  # Nix packages rustc and rust-src separately, so RUST_LIB_SRC points
  # to a different store path than rustc's sysroot. The kernel's
  # generate_rust_analyzer.py asserts they share a common root.
  #
  # Work around this by creating a sysroot that includes the library
  # sources at the expected location, and wrapping rustc to report it.
  rustLibSrc = pkgs.rustPlatform.rustLibSrc;

  rustSysroot = pkgs.runCommand "rust-sysroot-with-src" {} ''
    orig=$(${pkgs.rustc}/bin/rustc --print sysroot)
    mkdir -p $out

    # Symlink everything except lib
    for f in "$orig"/*; do
      [ "$(basename "$f")" = "lib" ] && continue
      ln -s "$f" "$out/$(basename "$f")"
    done

    # Recreate lib tree, symlinking contents but carving out
    # the path where we need to add rust-src
    mkdir -p $out/lib/rustlib/src/rust
    for f in "$orig"/lib/*; do
      [ "$(basename "$f")" = "rustlib" ] && continue
      ln -s "$f" "$out/lib/$(basename "$f")"
    done
    for f in "$orig"/lib/rustlib/*; do
      [ "$(basename "$f")" = "src" ] && continue
      ln -s "$f" "$out/lib/rustlib/$(basename "$f")"
    done

    ln -s ${rustLibSrc} $out/lib/rustlib/src/rust/library
  '';

  rustcWrapped = pkgs.runCommand "rustc-wrapped" {} ''
    mkdir -p $out/bin
    for tool in ${pkgs.rustc}/bin/*; do
      name=$(basename "$tool")
      cat > "$out/bin/$name" <<WRAPPER
#!/bin/sh
if [ "\$1" = "--print" ] && [ "\$2" = "sysroot" ]; then
  echo "${rustSysroot}"
  exit 0
fi
exec "$tool" --sysroot "${rustSysroot}" "\$@"
WRAPPER
      chmod +x "$out/bin/$name"
    done
  '';
in

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    # kernel build essentials
    gnumake gcc clang binutils
    flex bison bc
    pkg-config pahole
    elfutils ncurses openssl zlib

    # Rust for kernel (wrapped to include rust-src in sysroot)
    rustcWrapped cargo rust-bindgen rust-analyzer
    llvmPackages.libclang llvmPackages.lld

    # test infrastructure
    qemu
    brotli lcov
    python3
    gdb socat
    perl
  ];

  RUST_LIB_SRC = "${rustSysroot}/lib/rustlib/src/rust/library";
}
