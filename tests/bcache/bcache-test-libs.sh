#
# Library with some functions for writing bcache tests using the
# ktest framework.
#

require-lib ../test-libs.sh

require-bin make-bcache
require-bin bcache-super-show
require-bin bcachectl

require-kernel-config BCACHE,BCACHE_DEBUG,CLOSURE_DEBUG

SYSFS=""
BDEV=""
CACHE=""
TIER=""
VOLUME=""
DISCARD=1
WRITEBACK=0
REPLACEMENT=lru

VIRTIO_BLKDEVS=0

DATA_REPLICAS=1
META_REPLICAS=1

#
# Bcache configuration
#
config-backing()
{
    add_bcache_devs BDEV $1
}

config-cache()
{
    add_bcache_devs CACHE $1
}

config-tier()
{
    add_bcache_devs TIER $1
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
    BUCKET_SIZE="$1"
}

config-block-size()
{
    BLOCK_SIZE="$1"
}

config-writeback()
{
    WRITEBACK=1
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
    done
}

make_bcache_flags()
{
    flags="--bucket $BUCKET_SIZE --block $BLOCK_SIZE --cache_replacement_policy=$REPLACEMENT"
    case "$DISCARD" in
	0) ;;
	1) flags+=" --discard" ;;
	*) echo "Bad discard: $DISCARD"; exit ;;
    esac
    case "$WRITEBACK" in
	0) ;;
	1) flags+=" --writeback" ;;
	*) echo "Bad writeback: $WRITEBACK"; exit ;;
    esac
    echo $flags
}

add_device() {
    DEVICES="$DEVICES /dev/bcache$DEVICE_COUNT"
    DEVICE_COUNT=$(($DEVICE_COUNT + 1))
}

wait_on_dev()
{
    for device in $@; do
	while [ ! -b "$device" ]; do
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

    # Make sure bcache-super-show works -- the control plane wipes data
    # if this fails so its important that it doesn't break
    for dev in $CACHE $BDEV $TIER; do
	bcache-super-show $dev
    done

    # Older kernel versions don't have /dev/bcache
    if [ -e /dev/bcache ]; then
	bcachectl register $CACHE $TIER $BDEV
    else
	for dev in $CACHE $TIER $BDEV; do
	    echo $dev > /sys/fs/bcache/register
	done
    fi

    # If we have one or more backing devices, then we get
    # one bcacheN per backing device.
    for device in $BDEV; do
	add_device
    done

    udevadm settle

    for device in $DEVICES; do
	wait_on_dev $device
    done

    cache_set_settings

    # Set up flash-only volumes.
    for volume in $VOLUME; do
	add_device
    done

    cached_dev_settings

    eval "$SYSFS"
}

#
# Registers all bcache devices after running make-bcache.
#
setup_bcache() {
    make_bcache_flags="$(make_bcache_flags)"
    make_bcache_flags+=" --wipe-bcache --cache $CACHE"
    make_bcache_flags+=" --data-replicas $DATA_REPLICAS"
    make_bcache_flags+=" --meta-replicas $META_REPLICAS"

    if [ "$TIER" != "" ]; then
	make_bcache_flags+=" --tier 1 $TIER"
    fi

    if [ "$BDEV" != "" ]; then
	make_bcache_flags+=" --bdev $BDEV"
    fi

    # Let's change the checksum type just for fun
    make-bcache --csum-type crc32c $make_bcache_flags

    existing_bcache

    for size in $VOLUME; do
	for file in /sys/fs/bcache/*/flash_vol_create; do
	    echo $size > $file
	done
    done
}

stop_volumes()
{
    for dev in /sys/block/bcache*/bcache/unregister; do
	echo > $dev
    done
}

stop_bcache()
{
    echo 1 > /sys/fs/bcache/reboot
}

cache_set_settings()
{
    for dir in $(ls -d /sys/fs/bcache/*-*-*); do
	true
	echo 1 > $dir/btree_scan_ratelimit

	#echo 0 > $dir/synchronous
	echo panic > $dir/errors

	#echo 0 > $dir/journal_delay_ms
	#echo 1 > $dir/internal/key_merging_disabled
	#echo 1 > $dir/internal/btree_coalescing_disabled
	#echo 1 > $dir/internal/verify

	# This only exists if CONFIG_BCACHE_DEBUG is on
	if [ -f $dir/internal/expensive_debug_checks ]; then
	    echo 1 > $dir/internal/expensive_debug_checks
	fi

	echo 0 > $dir/congested_read_threshold_us
	echo 0 > $dir/congested_write_threshold_us

	echo 1 > $dir/internal/copy_gc_enabled

	# Disable damping effect since test cache devices are so small
	echo 1 > $dir/internal/tiering_rate_p_term_inverse
	echo 1 > $dir/internal/foreground_write_rate_p_term_inverse
	for dev in $(ls -d $dir/cache[0-9]*); do
	    echo 1 > $dev/copy_gc_rate_p_term_inverse
	done
    done
}

cached_dev_settings()
{
    for dir in $(ls -d /sys/fs/bcache/*-*-*/bdev*); do
	echo 1 > $dir/writeback_rate_p_term_inverse
    done
}

setup_bcachefs()
{
    uuid=$(ls -d /sys/fs/bcache/*-*-* | sed -e 's/.*\///')
    mkdir -p /mnt/bcachefs
    mount -t bcachefs $uuid /mnt/bcachefs

    # for fs workloads to know mount point
    DEVICES=bcachefs
}

stop_bcachefs()
{
    umount /mnt/bcachefs
}

test_bcachefs_stress()
{
    setup_bcachefs
    test_dbench
    test_bonnie
    #test_fsx
    stop_bcachefs
}
