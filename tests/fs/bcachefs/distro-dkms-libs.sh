#!/usr/bin/env bash
#
# Build bcachefs as a DKMS module inside a distro's own root filesystem.
#
# WHAT THIS IS FOR
# bcachefs ships as DKMS, so users compile it with their distro's toolchain,
# not ours - and the difference is not cosmetic. Arch builds its kernel with
# GCC 16.1.1 and CachyOS builds its with Clang 22.1.8 ThinLTO. Compile a
# module for Arch's kernel with GCC 15 and the *kernel's own* blkdev.h fails
# on __counted_by_ptr, an error no Arch user will ever see. Booting a distro
# kernel under ktest's Debian root image does not help either: the module
# still gets built by Debian's compiler.
#
# koverstreet/bcachefs#1197 was three distros reporting DKMS builds that
# produced no module at all - a bcachefs root then drops to an emergency shell
# at the next mkinitcpio - and nothing in ktest's matrix could see any of it.
#
# WHERE THE ROOTFS COMES FROM
# distro-kernel-fetcher/rootfs/<distro>: a nix derivation that extracts a
# pinned, hashed package set. Built host-side (the guest has no nix) and
# reached through the /nix/store virtiofs export.
#
# NOT COVERED YET
# The module is built against the rootfs's own kernel headers, not the kernel
# the fetcher downloaded, so this proves "builds with the distro's toolchain",
# not "builds for the kernel a user runs" - and it cannot insmod what it
# produces. Pairing the two is the next step.

. "$(dirname "$(readlink -e "${BASH_SOURCE[0]}")")/../../test-libs.sh"

config-timeout $((45 * 60))
require-kernel-config OVERLAY_FS

require-git https://evilpiepirate.org/git/bcachefs-tools.git

DISTRO_ROOTFS_NIX=$(dirname "$(readlink -e "${BASH_SOURCE[0]}")")/../../../../distro-kernel-fetcher/rootfs

# Build the rootfs on the host and leave its store path where the guest can
# read it. Callers run this at file scope: it has to happen host-side, since
# the test body runs in the VM and the VM has no nix. `deps` and `list-tests`
# are separate host-side invocations so it runs more than once - nix-build is
# a cache hit after the first.
#
# ktest_out is exported into the deps subprocess, which is what makes the
# handoff possible; the deps protocol itself carries only a fixed list of
# ktest_* names, so a new variable would not survive it.
distro_rootfs_prepare()
{
    local distro=$1

    command -v nix-build > /dev/null 2>&1			|| return 0
    [[ -n ${ktest_out:-} && -d ${ktest_out:-} ]]		|| return 0
    [[ -d $DISTRO_ROOTFS_NIX/$distro ]]				|| return 0

    nix-build --no-out-link "$DISTRO_ROOTFS_NIX/$distro" \
	> "$ktest_out/$distro-rootfs-path" 2>&1 ||
	rm -f "$ktest_out/$distro-rootfs-path"
}

# $1 distro, $2 expected number of Rust objects (of 3), rest: extra make args
distro_dkms_build()
{
    local distro=$1 want_rust=$2; shift 2
    local make_args=("$@")

    if [[ ! -f /ktest-out/$distro-rootfs-path ]]; then
	echo "FAIL: no $distro rootfs was built on the host"
	echo "expected nix-build of distro-kernel-fetcher/rootfs/$distro to"
	echo "leave its store path in \$ktest_out/$distro-rootfs-path - is"
	echo "nix-build on the host's PATH, and the fetcher checked out"
	echo "beside ktest?"
	return 1
    fi
    local rootfs=$(tail -1 "/ktest-out/$distro-rootfs-path")

    if [[ ! -d $rootfs ]]; then
	echo "FAIL: rootfs $rootfs is not readable in the guest"
	echo "/nix/store is a separate mount on the host and virtiofs does not"
	echo "cross submounts, so it needs its own export - see libktest.sh."
	if mountpoint -q /nix/store; then
	    echo "(/nix/store IS mounted, so the path itself is wrong)"
	else
	    echo "(/nix/store is NOT mounted in the guest)"
	fi
	return 1
    fi
    echo "rootfs: $rootfs"
    # Guarded, not `2>/dev/null`: sed exits 2 on a missing file and errexit
    # acts on that whether or not the message was hidden.
    if [[ -f $rootfs/.rootfs-info ]]; then
	sed 's/^/  /' "$rootfs/.rootfs-info"
    fi

    # overlay rather than copy: the store path is read-only and ~1-2G, and
    # every run wants a clean writable tree. The lower layer is shared and
    # never modified.
    local root=/distro-root
    mkdir -p $root /run/distro
    mount -t tmpfs tmpfs /run/distro
    mkdir -p /run/distro/upper /run/distro/work
    mount -t overlay overlay \
	-o lowerdir=$rootfs,upperdir=/run/distro/upper,workdir=/run/distro/work \
	$root

    mkdir -p $root/proc $root/sys $root/dev $root/build
    mount --bind /proc $root/proc
    mount --bind /sys  $root/sys
    mount --bind /dev  $root/dev

    local tools="${BCACHEFS_TOOLS_DIR:-$ktest_deps_dir/bcachefs-tools}"
    if ! make -s -C "$tools" install_dkms DESTDIR=$root/stage > /tmp/install_dkms.log 2>&1; then
	echo "FAIL: install_dkms failed"
	tail -20 /tmp/install_dkms.log
	return 1
    fi
    mv $root/stage/usr/src/bcachefs-* $root/build/src

    cat > $root/build.sh <<CONTAINER
#!/bin/bash
# /.rootfs-env carries what entering this rootfs requires: PATH, ldconfig,
# LIBCLANG_PATH. Each of those was a failure first.
. /.rootfs-env
K=\$(echo /usr/lib/modules/*/build)
echo "container: kernel \$(basename "\$(dirname "\$K")")"
# Report whichever compilers exist rather than guessing at one: \`cc --version
# | head -1 || fallback\` cannot work, because the pipeline's status is head's
# and the || never fires.
for c in gcc clang; do
    command -v \$c > /dev/null || continue
    \$c --version | head -1 | sed 's/^/container: /'
done
make ${make_args[*]} -j\$(nproc) -C "\$K" M=/build/src modules > /build/build.log 2>&1
echo "container: make exited \$?"
CONTAINER
    chmod +x $root/build.sh

    # env -i is load-bearing, not tidiness. A chroot inherits the caller's
    # whole environment, and on a nix host that is dense with store paths
    # which do not exist inside it. RUST_LIB_SRC is the quiet one: the
    # kernel's rust_is_available.sh honours it, a host store path makes it
    # report Rust unavailable, and the module then builds C-only and passes.
    chroot $root /usr/bin/env -i \
	PATH=/usr/bin:/usr/sbin:/bin:/sbin HOME=/root TERM=dumb \
	/build.sh

    local log=$root/build/build.log

    # Dump the log rather than grepping it for what we guess the error was.
    # ktest sets pipefail (lib/common.sh), so a grep that matches nothing is
    # itself fatal - which is how you get a test that dies on its own failure
    # path and never says what failed. A build log is small; print it.
    dump_build_log()
    {
	echo "--- tail of $log ---"
	if [[ -f $log ]]; then
	    tail -40 "$log"
	else
	    echo "(no build log - the build did not get far enough to write one)"
	fi
	echo "--- end ---"
    }

    local ko=$root/build/src/src/fs/bcachefs/bcachefs.ko
    if [[ ! -f $ko ]]; then
	echo "FAIL: no bcachefs.ko - the DKMS build produced no module"
	echo "this is #1197's failure mode: mkinitcpio then says"
	echo "'module not found: bcachefs' and a bcachefs root will not boot"
	dump_build_log
	return 1
    fi

    # Counted with a loop, not `ls a b c | wc -l`: under pipefail an ls that
    # cannot find one of its arguments exits 2 and takes the test with it,
    # before it can report the count it was trying to measure.
    local d=$root/build/src/src/fs/bcachefs rust_objs=0 o
    for o in "$d/mod.o" "$d/uuid.o" "$d/rust/extern.o"; do
	if [[ -f $o ]]; then rust_objs=$((rust_objs + 1)); fi
    done

    if (( rust_objs != want_rust )); then
	echo "FAIL: built $rust_objs/3 Rust objects, expected $want_rust"
	if (( rust_objs < want_rust )); then
	    echo "the toolchain is complete, so the probe declined - every"
	    echo "Rust-path bug would go untested behind a green run"
	fi
	dump_build_log
	return 1
    fi

    echo "PASS: bcachefs.ko built with $distro's toolchain" \
	 "($(stat -c %s "$ko") bytes, $rust_objs/3 Rust objects)"
}
