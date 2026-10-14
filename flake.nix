{
  description = "ktest - virtual machine test framework for kernel and filesystem testing";

  inputs = {
    # farm-nixos takes this flake as an input and makes it follow the fleet's
    # nixpkgs; this one is the dev shell's (build-test-kernel's toolchain).
    nixpkgs.url = github:NixOS/nixpkgs;
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # The host environment build-test-kernel needs: kernel builds (incl.
        # CONFIG_RUST and host tools like objtool/sign-file), the rust-analyzer
        # test, and the qemu test runtime. Without this, btk builds depend on
        # whatever the invoking shell's profile happens to carry — missing
        # python3/libelf/gmp surface as buried mid-build failures.
        kernelBuildDeps = with pkgs; [
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

        # What the scripts themselves run, beyond the kernel toolchain.
        # sudo and systemd-nspawn (upstream-kernel-build's build container)
        # are the host's: the wrappers prefix PATH, they don't replace it.
        scriptDeps = with pkgs; [
          bash coreutils findutils gnugrep gnused gawk diffutils which file
          gnutar gzip util-linux procps
          git jq openssh curl wget
        ];

        # ktest as a package: its tree in the store, and wrappers for the
        # scripts run from outside a VM. The scripts find the rest of ktest
        # relative to themselves (readlink -f $0), so they run from here as
        # from a checkout - reading the tree only; their state is ~/.ktest.
        #
        # Shebangs are left alone: tests/ and lib/ run inside the VM too,
        # against the guest's /usr/bin/env, where a /nix/store path doesn't
        # resolve.
        ktest = pkgs.stdenv.mkDerivation {
          pname = "ktest";
          version = self.shortRev or self.dirtyShortRev or "dev";
          src = self;

          nativeBuildInputs = [ pkgs.makeWrapper ];
          dontPatchShebangs = true;

          # libktest.sh builds the supervisor on demand next to itself, which
          # a read-only store can't take: build it here. (lwip-connect, for
          # vde networking only, needs liblwipv6 and isn't built.)
          buildPhase = ''
            runHook preBuild
            make -C lib supervisor
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p $out/share/ktest $out/bin
            cp -a . $out/share/ktest/
            for s in ktest build-test-kernel upstream-kernel-build \
                     kernel-store-producer ktest-deploy; do
              makeWrapper $out/share/ktest/$s $out/bin/$s \
                --prefix PATH : ${pkgs.lib.makeBinPath (scriptDeps ++ kernelBuildDeps)}
            done
            runHook postInstall
          '';

          meta = {
            description = "ktest - virtual machine test framework";
            mainProgram = "ktest";
          };
        };
      in {
        packages = {
          inherit ktest;
          default = ktest;
        };

        devShells.default = pkgs.mkShell {
          packages = kernelBuildDeps;
          # In-tree rust builds compile core from source:
          RUST_LIB_SRC = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
}
