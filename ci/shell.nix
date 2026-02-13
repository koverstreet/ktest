# CI worker Rust environment.
#
# Provides the Rust toolchain with a merged sysroot (rust-src at the
# path generate_rust_analyzer.py expects). System compilers (gcc, clang)
# come from configuration.nix — do NOT add them here, as nix-shell's
# cc-wrapper adds hardening flags that break kernel builds.
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

pkgs.mkShellNoCC {
  nativeBuildInputs = with pkgs; [
    # Rust for kernel (wrapped to include rust-src in sysroot)
    rustcWrapped cargo rust-bindgen rust-analyzer

    # Do NOT include llvmPackages.libclang in nativeBuildInputs — its
    # setup hooks set LLVM environment variables that cause the system
    # clang to warn about unused flags, which becomes fatal with -Werror.
    # Set LIBCLANG_PATH directly instead.
  ];

  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  RUST_LIB_SRC = "${rustSysroot}/lib/rustlib/src/rust/library";
}
