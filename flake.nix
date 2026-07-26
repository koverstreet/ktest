{
  description = "Flake for ktest kernel builds";

  inputs = {
    nixpkgs.url = github:NixOS/nixpkgs;
    utils.url = "github:numtide/flake-utils";
    src.url = "https://evilpiepirate.org/git/bcachefs.git";
    src.flake = false;
    buildRoot.url = "path:dummy-kernel-build";
    buildRoot.flake = false;
  };

  outputs = { self, utils, src,
              buildRoot,
              nixpkgs }:

    utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        ## TODO plumb kernel version automatically in to builds.
        preBuiltKernel = pkgs.callPackage ./kernel_install.nix {
                            inherit src buildRoot;
                          };
        srcBuildKernel =  pkgs.buildLinux { inherit src; };
      in {
        packages = {
          inherit preBuiltKernel srcBuildKernel;
          default = if (import buildRoot).isPreBuilt then preBuiltKernel else srcBuildKernel;
        };

        # The host environment build-test-kernel needs: kernel builds (incl.
        # CONFIG_RUST and host tools like objtool/sign-file), the rust-analyzer
        # test, and the qemu test runtime. Without this, btk builds depend on
        # whatever the invoking shell's profile happens to carry — missing
        # python3/libelf/gmp surface as buried mid-build failures.
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # kernel build
            gcc gnumake flex bison bc perl pahole
            elfutils openssl ncurses pkg-config
            gmp libmpc mpfr             # gcc plugin headers
            cpio zstd xz kmod rsync
            python3
            # rust kernel support + rust-analyzer test
            rustc rust-bindgen rust-analyzer clang
            # test runtime
            qemu_kvm socat
          ];
          # In-tree rust builds compile core from source:
          RUST_LIB_SRC = "${pkgs.rustPlatform.rustLibSrc}";
        };
    });
}
