# FHS shell for running ./root_image on NixOS
# execute:
# $ nix-shell root_image.nix
# $ ./root_image <params>
# $ exit

{ pkgs ? import <nixpkgs> {} }:
(pkgs.buildFHSUserEnv {
  name = "root_image-env";
  targetPkgs = pkgs: with pkgs; [
    coreutils
  ];
  runScript = "bash";
}).env
