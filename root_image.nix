# FHS shell for running ./root_image on NixOS
# execute:
# $ sudo nix-shell root_image.nix
# $ ./root_image create
# $ exit
#
# sudo outside rather than inside: root_image refuses to run as anyone else,
# and sudo from within the FHS env leaves it again.
#
# targetPkgs is what ./root_image and the vendored debootstrap call by name,
# taken from what they actually invoke rather than from whatever a Debian host
# happens to have lying around:
#
#   root_image  - fallocate, mount, umount (util-linux), chroot (coreutils),
#                 mkfs.ext4 (e2fsprogs), curl, rsync
#   debootstrap - wget, ar (binutils), tar, xz, gpgv (gnupg), dpkg-deb (dpkg),
#                 perl
{ pkgs ? import <nixpkgs> {} }:
(pkgs.buildFHSEnv {
  name = "root_image-env";
  targetPkgs = pkgs: with pkgs; [
    coreutils
    util-linux
    e2fsprogs
    curl
    wget
    rsync
    binutils
    gnutar
    xz
    gzip
    gnupg
    dpkg
    perl
    debianutils
  ];
  runScript = "bash";
}).env
