#
# Library with some functions for writing bcache tests using the
# ktest framework.
#

require-lib ../test-libs.sh
require-build-deb bcache-tools

require-kernel-config MD
require-kernel-config BCACHE,BCACHE_DEBUG

if [[ $KERNEL_ARCH = x86 ]]; then
    require-kernel-config CRYPTO_CRC32C_INTEL
    require-kernel-config CRYPTO_POLY1305_X86_64
    require-kernel-config CRYPTO_CHACHA20_X86_64
fi

#Expensive:
#require-kernel-config CLOSURE_DEBUG

SYSFS=""
BDEV=""
CACHE=""
VOLUME=""
WRITEBACK=0
WRITEAROUND=0
REPLACEMENT=lru
BUCKET_SIZE=""
BLOCK_SIZE=""

VIRTIO_BLKDEVS=0

DATA_REPLICAS=1
META_REPLICAS=1

#
# Bcache configuration
#
config-backing()
{
    add_bcache_devs BDEV $1 1
}

config-cache()
{
    add_bcache_devs CACHE $1 0
}

config-volume()
{
    for size in $(echo $1 | tr ',' ' '); do
	if [ "$VOLUME" == "" ]; then
	    VOLUME=" "
	fi
	VOLUME+="$size"
    done
}

config-bucket-size()
{
    BUCKET_SIZE="--bucket=$1"
}

config-block-size()
{
    BLOCK_SIZE="--block=$1"
}

config-writeback()
{
    WRITEBACK=1
}

config-writearound()
{
    WRITEAROUND=1
}

config-replacement()
{
    REPLACEMENT="$1"
}

config-data-replicas()
{
    DATA_REPLICAS="$1"
}

config-meta-replicas()
{
    META_REPLICAS="$1"
}

config-bcache-sysfs()
{
    if [ "$SYSFS" != "" ]; then
	SYSFS+="; "
    fi
    SYSFS+="for file in /sys/fs/bcache/*/$1; do echo $2 > \$file; done"
}

# Scratch devices are sdb onwards
get_next_virtio()
{
    # Ugh...
    letter="$(printf "\x$(printf "%x" $((98 + $VIRTIO_BLKDEVS)))")"
    echo "/dev/sd$letter"
}

# Usage: add_bcache_devs <variable> <sizes> <rotational>
add_bcache_devs()
{
    for size in $(echo $2 | tr ',' ' '); do
	config-scratch-devs $size

	if [ "$(eval echo \$$1)" != "" ]; then
	    eval $1+='" "'
	fi
	dev="$(get_next_virtio)"
	VIRTIO_BLKDEVS=$(($VIRTIO_BLKDEVS + 1))
	eval $1+="$dev"
	if [ "$TEST_RUNNING" != "" ]; then
	    echo "$3" > "/sys/block/$(basename "$dev")/queue/rotational"
	fi
    done
}

bcache_format()
{
    make-bcache $BUCKET_SIZE $BLOCK_SIZE -C $CACHE -B $BDEV
}

add_device() {
    DEVICES="$DEVICES /dev/bcache$DEVICE_COUNT"
    DEVICE_COUNT=$(($DEVICE_COUNT + 1))
}

wait_on_dev()
{
    for device in $@; do
	while [ ! -b "$device" ] && [ ! -c "$device" ]; do
	    sleep 0.5
	done
    done
}

#
# Registers all bcache devices.
#
# Upon successful completion, the DEVICES variable is set to a list of
# bcache block devices.
#
existing_bcache() {
    DEVICES=
    DEVICE_COUNT=0

    echo "registering via sysfs"

    for dev in $CACHE $BDEV; do
	echo $dev > /sys/fs/bcache/register
    done

    echo "registered"

    # If we have one or more backing devices, then we get
    # one bcacheN per backing device.
    for device in $BDEV; do
	add_device
    done

    udevadm settle

    echo -n "setting cache set settings: "
    cache_set_settings
    echo done

    echo -n "creating volumes: "
    for volume in $VOLUME; do
	add_device
    done
    echo done

    echo -n "setting backing device settings: "
    cached_dev_settings
    echo done

    echo -n "doing sysfs test: "
    eval "$SYSFS"
    echo done
}

#
# Registers all bcache devices after running make-bcache.
#
setup_bcache() {
    bcache_format

    existing_bcache
    sleep 2

    for size in $VOLUME; do
	for file in /sys/fs/bcache/*/blockdev_volume_create; do
	    echo "creating volume $size via $file"
	    echo $size > $file
	done
    done

    ln -s /sys/fs/bcache/*-* /root/c || true
}

stop_volumes()
{
    for dev in /sys/block/bcache*/bcache/unregister; do
	echo 1 > $dev
    done
    sleep 1
}

stop_bcache()
{
    for dev in /sys/fs/bcache/*/unregister; do
	echo 1 > $dev
    done
}

cache_set_settings()
{
    for dir in $(ls -d /sys/fs/bcache/*-*-*); do
	true
	#echo panic > $dir/errors

	#echo 0 > $dir/journal_delay_ms
	#echo 1 > $dir/internal/key_merging_disabled
	#echo 1 > $dir/internal/btree_coalescing_disabled
	#echo 1 > $dir/internal/verify

	echo foo1
	echo 0 > $dir/congested_read_threshold_us
	echo 0 > $dir/congested_write_threshold_us

	echo foo2
	#echo 1 > $dir/internal/copy_gc_enabled

	# Disable damping effect since test cache devices are so small

	#[[ -f $dir/internal/tiering_rate_p_term_inverse ]] &&
	#    echo 1 > $dir/internal/tiering_rate_p_term_inverse

	echo foo3
	[[ -f $dir/internal/foreground_write_rate_p_term_inverse ]] &&
	    echo 1 > $dir/internal/foreground_write_rate_p_term_inverse

	#for dev in $(ls -d $dir/cache[0-9]*); do
	#    [[ -f $dev/copy_gc_rate_p_term_inverse ]] &&
	#	echo 1 > $dev/copy_gc_rate_p_term_inverse
	#done

	echo foo4
    done
}

cached_dev_settings()
{
    for dir in $(ls -d /sys/fs/bcache/*-*-*/bdev*); do
	echo 1 > $dir/writeback_rate_p_term_inverse
    done
}

expect_sysfs()
{
    prefix=$1
    name=$2
    value=$3

    for file in $(echo /sys/fs/bcache/*/${prefix}*/${name}); do
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

test_sysfs()
{
    echo -n "test_sysfs(): "

    if [ -d /sys/fs/bcache/*-* ]; then
	find -H /sys/fs/bcache/ -type f -perm -0400 -exec cat {} \; \
	    > /dev/null
	find -H /sys/block/*/bcache/ -type f -perm -0400 -exec cat {} \; \
	    > /dev/null
    fi

    echo done
}

antagonist_shrink()
{
    while true; do
	for file in $(find /sys/fs/bcache -name prune_cache); do
	    echo 100000 > $file
	done
	sleep 0.5
    done
}

antagonist_expensive_debug_checks()
{
    # This only exists if CONFIG_BCACHE_DEBUG is on
    p=/sys/module/bcache/parameters/expensive_debug_checks

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
	echo 1 | tee /sys/fs/bcache/*/internal/trigger_gc > /dev/null 2>&1 || true
    done
}

test_antagonist()
{
    antagonist_expensive_debug_checks &
    antagonist_shrink &
    antagonist_sync &
    antagonist_trigger_gc &
}

test_discard()
{
    if [ "${BDEV:-}" == "" -a "${CACHE:-}" == "" ]; then
        return
    fi

    killall -STOP systemd-udevd

    if [ -f /sys/kernel/debug/bcache/* ]; then
	cat /sys/kernel/debug/bcache/* > /dev/null
    fi

    for dev in $DEVICES; do
        echo "Discarding ${dev}..."
        blkdiscard $dev
    done

    if [[ -f /sys/fs/bcache/*/internal/btree_gc_running ]]; then
	# Wait for btree GC to finish so that the counts are actually up to date
	while [ "$(cat /sys/fs/bcache/*/internal/btree_gc_running)" != "0" ]; do
	    sleep 1
	done
    fi

    expect_sysfs cache dirty_buckets 0
    expect_sysfs cache dirty_data 0
    expect_sysfs cache cached_buckets 0
    expect_sysfs cache cached_data 0
    expect_sysfs bdev dirty_data 0

    if [ -f /sys/kernel/debug/bcache/* ]; then
	tmp="$(mktemp)"
	cat /sys/kernel/debug/bcache/* | tee "$tmp"
	lines=$(grep -v discard "$tmp" | wc -l)

	if [ "$lines" != "0" ]; then
	    echo "Btree not empty"
	    false
	fi
    fi

    killall -CONT systemd-udevd
}

test_bcache_stress()
{
    echo "test_bcache_stress():"
    enable_faults

    test_sysfs
    test_fio
    test_discard

    setup_fs ext4
    test_dbench
    test_bonnie
    test_fsx
    stop_fs
    test_discard

    if [ $ktest_priority -gt 0 ]; then
	setup_fs xfs
	test_dbench
	test_bonnie
	stop_fs
	test_discard
    fi

    disable_faults
}
