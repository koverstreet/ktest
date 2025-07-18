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

# Set Lustre test-framework.sh environment
if [[ -f "$zfs_pkg_path/zfs" ]]; then
    export ZFS="$zfs_pkg_path/zfs"
    export ZPOOL="$zfs_pkg_path/zpool"
else
    export ZFS="$zfs_pkg_path/cmd/zfs/zfs"
    export ZPOOL="$zfs_pkg_path/cmd/zpool/zpool"
fi

export LUSTRE="$lustre_pkg_path/lustre"
export LCTL="$LUSTRE/utils/lctl"
export LNETCTL="$LUSTRE/../lnet/utils/lnetctl"
export RUNAS_ID="1000"

# Update paths
set +u
export PATH="$zfs_pkg_path:$zfs_pkg_path/cmd/zpool:$zfs_pkg_path/cmd/zfs:$PATH"
export LD_LIBRARY_PATH="$zfs_pkg_path/lib/libzfs/.libs:$zfs_pkg_path/lib/libzfs_core/.libs:$LD_LIBRARY_PATH"
export LD_LIBRARY_PATH="$zfs_pkg_path/lib/libuutil/.libs:$zfs_pkg_path/lib/libnvpair/.libs:$LD_LIBRARY_PATH"
set -u

# Dump out all of the special Lustre variables
function print_lustre_env() {
    echo "FSTYPE=$FSTYPE"
    echo "FSNAME=$FSNAME"
    echo "MGSNID=$MGSNID"
    echo "TESTSUITE=$TESTSUITE"
    echo "ONLY=$ONLY"
}

# Run a command as if it were part of test-framework.sh
function run_tf() {
	    cat << EOF | bash
. "$LUSTRE/tests/test-framework.sh" > /dev/null
init_test_env > /dev/null
$@
EOF
}

# Run llog_test.ko unit tests
function run_llog() {
    export MGS="$($LCTL dl | awk '/mgs/ { print $4 }')"

    cat << EOF | bash
. "$LUSTRE/tests/test-framework.sh" > /dev/null
init_test_env > /dev/null

# Load module
load_module kunit/llog_test || error "load_module failed"

# Using ignore_errors will allow lctl to cleanup even if the test fails.
$LCTL mark "Attempt llog unit tests"
eval "$LCTL <<-EOF || RC=2
	attach llog_test llt_name llt_uuid
	ignore_errors
	setup $MGS
	--device llt_name cleanup
	--device llt_name detach
EOF"
$LCTL mark "Finish llog units tests"
EOF
}

# Grab special Lustre environment variables
# TODO: There's probably a better way to do this...
set +u
if [[ -n "$FSTYPE" || -n "$FSNAME" || -n "$MGSNID" || -n "$TESTSUITE" || -n "$ONLY" ]]; then
    rm -f /tmp/ktest-lustre.env
    print_lustre_env > /tmp/ktest-lustre.env
else
    # If the filesystem doesn't exist, use defaults
    if [[ -f /host/tmp/ktest-lustre.env ]]; then
	eval $(cat /host/tmp/ktest-lustre.env)
    else
	FSTYPE="wbcfs"
    fi
fi
set -u

function configure_interface()
{
    local interface="$1"

    echo >> /etc/network/interfaces
    echo "auto $interface" >> /etc/network/interfaces
    echo "iface $interface inet dhcp" >> /etc/network/interfaces

    ip route del default
    ifup "$interface"
    dhclient "$interface"
    ip route show
}

function set_hostname_interface()
{
    local interface="$1"
    local local_ip="$(ip address show $interface | awk -F' ' '$1 == "inet" { print $2 }' | awk -F'/' '{ print $1 }')"
    local host_name="$(hostname)"

    sed -i '/$host_name/d' /etc/hosts
    echo "$local_ip" "$host_name" >> /etc/hosts
}

function load_zfs_modules()
{
    # ZFS pre-2.3.0
    insmod "$zfs_pkg_path/module/spl/spl.ko" || true
    insmod "$zfs_pkg_path/module/zstd/zzstd.ko" || true
    insmod "$zfs_pkg_path/module/unicode/zunicode.ko" || true
    insmod "$zfs_pkg_path/module/avl/zavl.ko" || true
    insmod "$zfs_pkg_path/module/lua/zlua.ko" || true
    insmod "$zfs_pkg_path/module/nvpair/znvpair.ko" || true
    insmod "$zfs_pkg_path/module/zcommon/zcommon.ko" || true
    insmod "$zfs_pkg_path/module/icp/icp.ko" || true
    insmod "$zfs_pkg_path/module/zfs/zfs.ko" || true

    # ZFS post-2.3.0
    insmod "$zfs_pkg_path/module/spl.ko" || true
    insmod "$zfs_pkg_path/module/zfs.ko" || true
}

function require-lustre-kernel-config()
{
    # Minimal config required for Lustre to build
    require-kernel-config QUOTA
    require-kernel-config KEYS
    require-kernel-config NETWORK_FILESYSTEMS
    require-kernel-config MULTIUSER
    require-kernel-config NFS_FS
    require-kernel-config BITREVERSE
    require-kernel-config CRYPTO_DEFLATE
    require-kernel-config ZLIB_DEFLATE
}

function require-lustre-debug-kernel-config()
{
    # Basic debugging stuff
    require-kernel-config KASAN
    require-kernel-config KASAN_VMALLOC

    # ZFS doesn't support some options
    if [[ "$FSTYPE" =~ "zfs" ]]; then
	return
    fi

    # Extra debug (probably expensive)
    require-kernel-config DEBUG_INFO
    require-kernel-config DEBUG_FS
    require-kernel-config DEBUG_KERNEL
    require-kernel-config DEBUG_MEMORY_INIT
    require-kernel-config DEBUG_RT_MUTEXES
    require-kernel-config DEBUG_SPINLOCK
    require-kernel-config DEBUG_MUTEXES
    require-kernel-config DEBUG_WW_MUTEX_SLOWPATH
    require-kernel-config DEBUG_RWSEMS
    require-kernel-config DEBUG_IRQFLAGS
    require-kernel-config DEBUG_BUGVERBOSE
    require-kernel-config DEBUG_PI_LIST
}

function require-lustre-efa-kernel-config()
{
    require-kernel-config NETDEVICES
    require-kernel-config PCI_MSI
    require-kernel-config NET_VENDOR_AMAZON
    require-kernel-config ETHERNET
    require-kernel-config PCI
    require-kernel-config AMAZON_DRIVER_UPDATES
    require-kernel-config AMAZON_ENA_ETHERNET
    require-kernel-config INFINIBAND
    require-kernel-config INFINIBAND_USER_ACCESS
    require-kernel-config AMAZON_EFA_INFINIBAND
}

function load_lustre_modules()
{
    if [[ "$FSTYPE" =~ "zfs" ]]; then
	load_zfs_modules
    fi

    FSTYPE="$FSTYPE" "$lustre_pkg_path/lustre/tests/llmount.sh" --load-modules
}

function setup_lustre_mgs()
{
    mkdir -p /mnt/lustre-mgs

    # TODO: This logic probably belongs in llmount.sh or test-framework.sh?
    case "$FSTYPE" in
	zfs)
	    zpool create lustre-mgs "${ktest_scratch_dev[0]}"
	    "$lustre_pkg_path/lustre/utils/mkfs.lustre" --mgs --fsname=lustre lustre-mgs/mgs
	    mount -t lustre lustre-mgs/mgs /mnt/lustre-mgs
	    ;;
	wbcfs)
	    export OSD_WBC_TGT_TYPE="MGT"
	    export OSD_WBC_INDEX="0"
	    export OSD_WBC_MGS_NID="$(hostname -i)@tcp"
	    export OSD_WBC_FSNAME="lustre"
	    run_tf "$lustre_pkg_path/lustre/utils/mount.lustre" -v lustre-wbcfs /mnt/lustre-mgs
	    ;;
	*)
	    echo "Unsupported OSD!"
	    exit 1
	    ;;
    esac
}

function cleanup_lustre_mgs()
{
    umount -t lustre /mnt/lustre-mgs
}

function configure_lnet()
{
    local efa_modpath="$LUSTRE/../lnet/klnds/efalnd/kefalnd.ko"
    local efa_interface="$(ls -1 /sys/class/infiniband | head -n1)"
    local eth_interface="$1"

    # Install EFA if available
    if [[ -f "$efa_modpath" ]]; then
	insmod "$efa_modpath" ipif_name="$eth_interface"
    fi

    # Reset configuration
    "$LNETCTL" set discovery 1
    "$LNETCTL" lnet configure
    "$LNETCTL" net del --net tcp || true

    # Add our TCP interfaces
    "$LNETCTL" net add --net tcp --if "$eth_interface" || true

    # Add our EFA interfaces
    if [[ -f "$efa_modpath" ]]; then
	"$LNETCTL" net add --net efa \
		   --if "$efa_interface" \
		   --peer-credits 128 || true
	"$LNETCTL" udsp add --src efa --priority 0
    fi

    # Dump config
    "$LNETCTL" net show -v
}

function lustre_performance_tuning()
{
    "$LCTL" set_param debug=0
}

function lustre_client_performance_tuning()
{
    lustre_performance_tuning

    "$LCTL" set_param llite.*.checksum_pages=0
    "$LCTL" set_param osc.*OST*.max_rpcs_in_flight=128
}

function setup_lustrefs()
{
    print_lustre_env
    load_lustre_modules

    FSTYPE="$FSTYPE" "$lustre_pkg_path/lustre/tests/llmount.sh"

    # Disable identity upcall (for OSD wbcfs)
    "$LCTL" set_param mdt.*.identity_upcall=NONE

    mount -t lustre
}

function cleanup_lustrefs()
{
    if [[ "$ktest_interactive" != "true" ]]; then
	FSTYPE="$FSTYPE" "$lustre_pkg_path/lustre/tests/llmountcleanup.sh"
    fi
}

# Lustre/ZFS will always taint kernel
allow_taint
