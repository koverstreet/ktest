#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0

#
# Copyright (c) 2024, Amazon and/or its affiliates. All rights reserved.
# Use is subject to license terms.
#

#
# Library for writing Lustre tests in the ktest format.
#
# Author: Timothy Day <timday@amazon.com>
#

. "$(dirname "$(dirname "$(dirname "$(readlink -e "${BASH_SOURCE[0]}")")")")/test-libs.sh"

# Currently, other packages must be in the same directory
# as the kernel source and ktest
export workspace_path="/workspace"
export lustre_pkg_path="$workspace_path/lustre-release"
export zfs_pkg_path="$workspace_path/zfs"

function load_zfs_modules()
{
    insmod "$zfs_pkg_path/module/spl/spl.ko"
    insmod "$zfs_pkg_path/module/zstd/zzstd.ko"
    insmod "$zfs_pkg_path/module/unicode/zunicode.ko"
    insmod "$zfs_pkg_path/module/avl/zavl.ko"
    insmod "$zfs_pkg_path/module/lua/zlua.ko"
    insmod "$zfs_pkg_path/module/nvpair/znvpair.ko"
    insmod "$zfs_pkg_path/module/zcommon/zcommon.ko"
    insmod "$zfs_pkg_path/module/icp/icp.ko"
    insmod "$zfs_pkg_path/module/zfs/zfs.ko"
}

# Set Lustre test-framework.sh environment
export ZFS="$zfs_pkg_path/cmd/zfs/zfs"
export ZPOOL="$zfs_pkg_path/cmd/zpool/zpool"
export FSTYPE="zfs"

# Update paths
set +u
export PATH="$zfs_pkg_path/cmd/zpool:$zfs_pkg_path/cmd/zfs:$PATH"
export LD_LIBRARY_PATH="$zfs_pkg_path/lib/libzfs/.libs:$zfs_pkg_path/lib/libzfs_core/.libs:$LD_LIBRARY_PATH"
export LD_LIBRARY_PATH="$zfs_pkg_path/lib/libuutil/.libs:$zfs_pkg_path/lib/libnvpair/.libs:$LD_LIBRARY_PATH"
set -u

# Lustre/ZFS will always taint kernel
allow_taint
