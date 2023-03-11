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
    });
}
