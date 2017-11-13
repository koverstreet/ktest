#
# Library with some functions for writing bcachefs tests using the
# ktest framework.
#

require-lib ../test-libs.sh
require-build-deb bcachefs-tools

require-kernel-config MD
require-kernel-config BCACHEFS_FS,BCACHEFS_DEBUG

if [[ $KERNEL_ARCH = x86 ]]; then
    require-kernel-config CRYPTO_CRC32C_INTEL
    require-kernel-config CRYPTO_POLY1305_X86_64
    require-kernel-config CRYPTO_CHACHA20_X86_64
fi

#Expensive:
#require-kernel-config CLOSURE_DEBUG

expect_sysfs()
{
    prefix=$1
    name=$2
    value=$3

    for file in $(echo /sys/fs/bcachefs/*/${prefix}*/${name}); do
        if [ -e $file ]; then
            current="$(cat $file)"
            if [ "$current" != "$value" ]; then
                echo "Mismatch for $file: got $current, want $value"
                exit 1
            else
                echo "OK: $file $value"
            fi
        fi
    done
}

read_all_sysfs()
{
    if [ -d /sys/fs/bcachefs/*-* ]; then
	find -H /sys/fs/bcachefs/ -type f -perm -0400 -exec cat {} \; \
	    > /dev/null
	find -H /sys/block/*/bcachefs/ -type f -perm -0400 -exec cat {} \; \
	    > /dev/null
    fi
}

antagonist_shrink()
{
    while true; do
	for file in $(find /sys/fs/bcachefs -name prune_cache); do
	    echo 100000 > $file
	done
	sleep 0.5
    done
}

antagonist_expensive_debug_checks()
{
    # This only exists if CONFIG_BCACHE_DEBUG is on
    p=/sys/module/bcachefs/parameters/expensive_debug_checks

    if [ -f $p ]; then
	while true; do
	    echo 1 > $p
	    sleep 5
	    echo 0 > $p
	    sleep 10
	done
    fi
}

antagonist_trigger_gc()
{
    while true; do
	sleep 5
	echo 1 | tee /sys/fs/bcachefs/*/internal/trigger_gc > /dev/null 2>&1 || true
    done
}

antagonist_switch_crc()
{
    cd /sys/fs/bcachefs

    while true; do
	sleep 1
	echo crc64 | tee */options/data_checksum	> /dev/null 2>&1 || true
	echo crc64 | tee */options/metadata_checksum	> /dev/null 2>&1 || true
	echo crc64 | tee */options/str_hash		> /dev/null 2>&1 || true
	sleep 1
	echo crc32c | tee */options/data_checksum	> /dev/null 2>&1 || true
	echo crc32c | tee */options/metadata_checksum	> /dev/null 2>&1 || true
	echo crc32c | tee */options/str_hash		> /dev/null 2>&1 || true
    done
}

run_antagonist()
{
    antagonist_expensive_debug_checks &
    antagonist_shrink &
    antagonist_sync &
    antagonist_trigger_gc &
    antagonist_switch_crc &
}

discard_all_devices()
{
    if [ "${BDEV:-}" == "" -a "${CACHE:-}" == "" ]; then
        return
    fi

    killall -STOP systemd-udevd

    if [ -f /sys/kernel/debug/bcachefs/* ]; then
	cat /sys/kernel/debug/bcachefs/* > /dev/null
    fi

    for dev in $DEVICES; do
        echo "Discarding ${dev}..."
        blkdiscard $dev
    done

    if [[ -f /sys/fs/bcachefs/*/internal/btree_gc_running ]]; then
	# Wait for btree GC to finish so that the counts are actually up to date
	while [ "$(cat /sys/fs/bcachefs/*/internal/btree_gc_running)" != "0" ]; do
	    sleep 1
	done
    fi

    expect_sysfs cache dirty_buckets 0
    expect_sysfs cache dirty_data 0
    expect_sysfs cache cached_buckets 0
    expect_sysfs cache cached_data 0
    expect_sysfs bdev dirty_data 0

    if [ -f /sys/kernel/debug/bcachefs/* ]; then
	tmp="$(mktemp)"
	cat /sys/kernel/debug/bcachefs/* | tee "$tmp"
	lines=$(grep -v discard "$tmp" | wc -l)

	if [ "$lines" != "0" ]; then
	    echo "Btree not empty"
	    false
	fi
    fi

    killall -CONT systemd-udevd
}

run_bcache_stress()
{
    enable_faults

    read_all_sysfs
    run_fio
    discard_all_devices

    setup_fs ext4
    run_dbench
    run_bonnie
    run_fsx
    stop_fs
    discard_all_devices

    if [ $ktest_priority -gt 0 ]; then
	setup_fs xfs
	run_dbench
	run_bonnie
	stop_fs
	discard_all_devices
    fi

    disable_faults
}

# some bcachefs tests:

setup_bcachefs()
{
    mkdir -p /mnt/bcachefs

    MNT=""
    for dev in $CACHE $TIER; do
	if [[ -z $MNT ]]; then
	    MNT=$dev
	else
	    MNT=$MNT:$dev
	fi
    done

    echo "mount -t bcachefs $MNT /mnt/bcachefs"
    mount -t bcachefs -o verbose_recovery $MNT /mnt/bcachefs

    # for fs workloads to know mount point
    DEVICES=bcachefs
}

stop_bcachefs()
{
    umount /mnt/bcachefs
}

run_bcachefs_stress()
{
    setup_bcachefs
    #enable_faults

    run_dbench
    run_bonnie
    run_fsx

    #disable_faults
    stop_bcachefs
}
