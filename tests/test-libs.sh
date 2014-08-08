#
# Library with some functions for writing bcache tests using the
# ktest framework.
#

require-bin make-bcache
require-bin bcachectl
require-kernel-config MD,BCACHE,BCACHE_DEBUG,CLOSURE_DEBUG
require-kernel-config DYNAMIC_FAULT

export PS4='+`basename ${BASH_SOURCE[0]}`:${LINENO}:${FUNCNAME[0]:+${FUNCNAME[0]}()}+ '

# Wait for an IP or IPv6 address to show
# up on a specific device.
# args: addr bits=24 type=4 dev=eth0 timeout=60 on=true
wait_on_ip()
{
    addr=${1:?"ERROR: address must be provided"}
    bits=${2:-"24"}
    addrtype=${3:-"4"}
    ethdev=${4:-"eth0"}
    timeout=${5:-"60"}
    on=${6:-"true"}

    case "$addrtype" in
    4)
	inet="inet"
	pingcmd="ping"
	;;
    6)
	inet="inet6"
	pingcmd="ping6"
	;;
    *)
	echo "ERROR: Unknown address type: $inet"
	exit 1
	;;
    esac

    i=0
    while true
    do
	ipinfo=$(ip -$addrtype -o addr show dev $ethdev)

	if [[ ( $on == "true" ) && ( $ipinfo =~ "$inet $addr/$bits" ) ]]
	then
	    $pingcmd -I $ethdev -c 1 $addr && break
	elif [[ ( $on == "false" ) && ! ( $ipinfo =~ "$inet $addr/$bits" ) ]]
	then
	    $pingcmd -I $ethdev -c 1 $addr || break
	fi

	if [ $i -gt $timeout ]
	then
	    exit 1
	fi

	i=$[ $i + 1 ]
	sleep 1
    done
}

wait_no_ip()
{
    wait_on_ip "$1" "$2" "$3" "$4" "$5" "false"
}

# Bcache setup

#
# Set up a block device without bcache.
#
setup_blkdev() {
    DEVICES=/dev/vda
}

add_device() {
    DEVICES="$DEVICES /dev/bcache$DEVICE_COUNT"
    DEVICE_COUNT=$(($DEVICE_COUNT + 1))
}

#
# Registers all bcache devices.
#
# Should be called after setting FLAGS, CACHE, BDEV and TIER variables
# FLAGS -- flags for make-bcache, such as --block, --discard, --writeback
# CACHE -- one or more cache devices in tier 0
# BDEV -- zero or more backing devices
# TIER -- zero or more cache devices in tier 1
# VOLUME -- zero or one sizes for flash only volume
#
# BDEV and VOLUME are mutually exclusive.
#
# Upon successful completion, the DEVICES variable is set to a list of
# bcache block devices.
#
existing_bcache() {
    bcachectl register $CACHE $TIER $BDEV

    DEVICES=
    DEVICE_COUNT=0

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
    for volume in ${VOLUME:=}; do
	add_device
    done

    cached_dev_settings
}

#
# Registers all bcache devices after running make-bcache.
#
setup_bcache() {
    make_bcache_flags="$FLAGS --wipe-bcache --cache $CACHE"

    if [ "$TIER" != "" ]; then
	make_bcache_flags="$make_bcache_flags --tier 1 $TIER"
    fi

    if [ "$BDEV" != "" ]; then
	make_bcache_flags="$make_bcache_flags --bdev $BDEV"
    fi

    make-bcache $make_bcache_flags

    existing_bcache

    for size in ${VOLUME:=}; do
	for file in /sys/fs/bcache/*/flash_vol_create; do
	    echo $size > $file
	done
    done
}

stop_fs()
{
    for dev in $DEVICES; do
	umount /mnt/$dev || true
    done

}

stop_bcache()
{
    echo 1 > /sys/fs/bcache/reboot
}

#
# Mount file systems on all bcache block devices.
# The FS variable should be set to one of the following:
# - none -- no file system setup, test doesn't need one
# - ext4 -- ext4 file system on bcache device
# - xfs -- xfs file system on bcache device
# - bcachefs -- bcachefs on cache set
#
existing_fs() {
    case $FS in
	ext4)
	    for dev in $DEVICES; do
		mkdir -p /mnt/$dev
		mount $dev /mnt/$dev -t ext4 -o errors=panic
	    done
	    ;;
	xfs)
	    for dev in $DEVICES; do
		mkdir -p /mnt/$dev
		mount $dev /mnt/$dev -t xfs -o wsync
	    done
	    ;;
	bcachefs)
	    # Hack -- when using bcachefs we don't have a backing
	    # device or a flash only volume, but we have to invent
	    # a name for the device for use as the mount point.
	    if [ "$DEVICES" != "" ]; then
		echo "Don't use a backing device or flash-only"
		echo "volume with bcachefs"
		exit 1
	    fi

	    dev=/dev/bcache0
	    DEVICES=$dev
	    uuid=$(ls -d /sys/fs/bcache/*-*-* | sed -e 's/.*\///')
	    echo "Mounting bcachefs on $uuid"
	    mkdir -p /mnt/$dev
	    mount -t bcachefs $uuid /mnt/$dev -o errors=panic
	    ;;
	*)
	    echo "Unsupported file system type: $FS"
	    exit 1
	    ;;
    esac

}

#
# Set up file systems on all bcache block devices and mount them.
#
setup_fs() {
    case $FS in
	ext4)
	    for dev in $DEVICES; do
		mkfs.ext4 $dev
	    done
	    ;;
	xfs)
	    for dev in $DEVICES; do
		mkfs.xfs $dev
	    done
	    ;;
	bcachefs)
	    ;;
	*)
	    echo "Unsupported file system type: $FS"
	    exit 1
	    ;;
    esac
    existing_fs
}

cache_set_settings()
{
    for dir in $(ls -d /sys/fs/bcache/*-*-*); do
	true
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
    done
}

cached_dev_settings()
{
    for dir in $(ls -d /sys/block/bcache*/bcache); do
	true
	#echo 128k    > $dir/readahead
	#echo 1	> $dir/writeback_delay
	#echo 0	> $dir/writeback_running
	#echo 0	> $dir/sequential_cutoff
	#echo 1	> $dir/verify
	#echo 1	> $dir/bypass_torture_test
    done
}

# Usage:
# setup_tracing buffer_size_kb tracepoint_glob
setup_tracing()
{
    echo > /sys/kernel/debug/tracing/trace
    echo $1 > /sys/kernel/debug/tracing/buffer_size_kb
    echo $2 > /sys/kernel/debug/tracing/set_event
    echo 1 > /proc/sys/kernel/ftrace_dump_on_oops
    echo 1 > /sys/kernel/debug/tracing/options/overwrite
    echo 1 > /sys/kernel/debug/tracing/tracing_on
}

dump_trace()
{
    cat /sys/kernel/debug/tracing/trace
}

# Bcache workloads
#
# The following variables must be set to use test_fio, test_bonnie or
# test_dbench:
# DEVICES - list of devices
# SIZE - one of small, medium or large

test_wait()
{
    for job in $(jobs -p); do
	wait $job
    done
}

test_bonnie()
{
    (
	case $SIZE in
	    small) loops=1 ;;
	    medium) loops=10 ;;
	    large) loops=100 ;;
	    *) exit 1 ;;
	esac

	for dev in $DEVICES; do
	    bonnie++ -x $loops -r 128 -u root -d /mnt/$dev &
	done

	test_wait
    )
}

test_dbench()
{
    (
	case $SIZE in
	    small) duration=30 ;;
	    medium) duration=300 ;;
	    large) duration=100000 ;;
	    *) exit 1 ;;
	esac

	for dev in $DEVICES; do
	    dbench -S -t $duration 2 -D /mnt/$dev &
	done

	test_wait
    )
}

test_fio()
{
    (
	# Our default working directory (/cdrom) is not writable,
	# fio wants to write files when verify_dump is set, so
	# change to a different directory.
	cd $LOGDIR

	case $SIZE in
	    small) loops=1 ;;
	    medium) loops=10 ;;
	    large) loops=100 ;;
	    *) exit 1 ;;
	esac

	for dev in $DEVICES; do
	    fio --eta=always - <<-ZZ &
		[global]
		randrepeat=0
		ioengine=libaio
		iodepth=64
		iodepth_batch=16
		direct=1

		numjobs=1

		verify_fatal=1
		verify_dump=1

		filename=$dev

		[seqwrite]
		loops=1
		blocksize_range=4k-128k
		rw=write
		verify=crc32c-intel

		[randwrite]
		stonewall
		blocksize_range=4k-128k
		loops=$loops
		rw=randwrite
		verify=meta
		ZZ
	done

	test_wait
    )
}

test_fsx()
{
    (
	case $SIZE in
	    small) numops=300000 ;;
	    medium) numops=3000000 ;;
	    large) numops=30000000 ;;
	    *) exit 1 ;;
	esac

	echo $DEVICES
	for dev in $DEVICES; do
	    ltp-fsx -N $numops /mnt/$dev/foo
	done

	test_wait
    )
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
                echo "Mismatch: $file $value != $current"
                exit 1
            else
                echo "OK: $file $value"
            fi
        fi
    done
}

test_discard()
{
    if [ "${BDEV:-}" == "" -a "${CACHE:-}" == "" ]; then
        return
    fi

    killall -STOP systemd-udevd

    for dev in $DEVICES; do
        echo "Discarding ${dev}..."
        blkdiscard $dev
    done

    sleep 1

    expect_sysfs cache dirty_buckets 0
    expect_sysfs cache dirty_data 0
    expect_sysfs cache cached_buckets 0
    expect_sysfs cache cached_data 0
    expect_sysfs bdev dirty_data 0

    killall -CONT systemd-udevd
}

# Bcache antagonists

test_sysfs()
{
    if [ -d /sys/fs/bcache/*-* ]; then
	find -H /sys/fs/bcache/ -type f -perm -0400 -exec cat {} \; \
	    > /dev/null
    fi
}

test_fault()
{
    [ -f /sys/kernel/debug/dynamic_fault/control ] || return

    while true; do
	echo "file alloc.c +o"	> /sys/kernel/debug/dynamic_fault/control
	echo "file btree.c +o"	> /sys/kernel/debug/dynamic_fault/control
	echo "file bset.c +o"	> /sys/kernel/debug/dynamic_fault/control
	echo "file io.c +o"	> /sys/kernel/debug/dynamic_fault/control
	echo "file journal.c +o"    > /sys/kernel/debug/dynamic_fault/control
	echo "file request.c +o"    > /sys/kernel/debug/dynamic_fault/control
	echo "file util.c +o"	> /sys/kernel/debug/dynamic_fault/control
	echo "file writeback.c +o"    > /sys/kernel/debug/dynamic_fault/control
	sleep 0.5
    done
}

test_shrink()
{
    while true; do
	for file in $(find /sys/fs/bcache -name prune_cache); do
	    echo 100000 > $file
	done
	sleep 0.5
    done
}

test_sync()
{
    while true; do
	sync
	sleep 0.5
    done
}

test_drop_caches()
{
    while true; do
	echo 3 > /proc/sys/vm/drop_caches
	sleep 5
    done
}

test_stress()
{
    test_sysfs

    test_shrink &
    test_fault &
    test_sync &
    test_drop_caches &

    test_fio

    setup_fs
    test_dbench
    test_bonnie
    stop_fs

    test_discard
}

test_powerfail()
{
    sleep 120
    echo b > /proc/sysrq-trigger
}

# Random stuff (that's not used anywhere AFAIK)

wait_on_dev()
{
    for device in $@; do
	while [ ! -b "$device" ]; do
	    sleep 0.5
	done
    done
}

setup_netconsole()
{
    IP=`ifconfig eth0|grep inet|sed -e '/inet/ s/.*inet addr:\([.0-9]*\).*$/\1/'`
    REMOTE=`cat /proc/cmdline |sed -e 's/^.*nfsroot=\([0-9.]*\).*$/\1/'`
    PORT=`echo $IP|sed -e 's/.*\(.\)$/666\1/'`

    mkdir	  /sys/kernel/config/netconsole/1
    echo $IP    > /sys/kernel/config/netconsole/1/local_ip
    echo $REMOTE    > /sys/kernel/config/netconsole/1/remote_ip
    echo $PORT    > /sys/kernel/config/netconsole/1/remote_port
    echo 1	> /sys/kernel/config/netconsole/1/enabled
}

setup_dynamic_debug()
{
    #echo "func btree_read +p"	> /sys/kernel/debug/dynamic_debug/control
    echo "func btree_read_work +p"	> /sys/kernel/debug/dynamic_debug/control

    echo "func btree_insert_recurse +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func btree_gc_recurse +p "    > /sys/kernel/debug/dynamic_debug/control

    #echo "func bch_btree_gc_finish +p "    > /sys/kernel/debug/dynamic_debug/control

    echo "func sync_btree_check +p "    > /sys/kernel/debug/dynamic_debug/control
    #echo "func btree_insert_keys +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func __write_super +p"	> /sys/kernel/debug/dynamic_debug/control
    echo "func register_cache_set +p"    > /sys/kernel/debug/dynamic_debug/control
    echo "func run_cache_set +p"	> /sys/kernel/debug/dynamic_debug/control
    echo "func write_bdev_super +p"	> /sys/kernel/debug/dynamic_debug/control
    echo "func detach_bdev +p"	> /sys/kernel/debug/dynamic_debug/control

    echo "func journal_read_bucket +p"    > /sys/kernel/debug/dynamic_debug/control
    echo "func bch_journal_read +p"	> /sys/kernel/debug/dynamic_debug/control
    echo "func bch_journal_mark +p"	> /sys/kernel/debug/dynamic_debug/control
    #echo "func bch_journal_replay +p"    > /sys/kernel/debug/dynamic_debug/control

    #echo "func btree_cache_insert +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func bch_btree_insert_check_key +p" > /sys/kernel/debug/dynamic_debug/control
    #echo "func cached_dev_cache_miss +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func request_read_done_bh +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func bch_insert_data_loop +p"    > /sys/kernel/debug/dynamic_debug/control

    #echo "func bch_refill_keybuf +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "func bcache_keybuf_next_rescan +p"    > /sys/kernel/debug/dynamic_debug/control
    #echo "file movinggc.c +p"	> /sys/kernel/debug/dynamic_debug/control
    #echo "file super.c +p"	    > /sys/kernel/debug/dynamic_debug/control
    #echo "func invalidate_buckets +p"    > /sys/kernel/debug/dynamic_debug/control

    #echo "file request.c +p"	> /sys/kernel/debug/dynamic_debug/control
}

setup_md_faulty()
{
    CACHE=md0
    #mdadm -C /dev/md0 -l6 -n4 /dev/vd[bcde]
    #mdadm -A /dev/md0 /dev/vd[bcde]

    #mdadm -B /dev/md0 -l0 -n2 /dev/vdb /dev/vdc

    mdadm -B /dev/md0 -lfaulty -n1 /dev/vda

    mdadm -G /dev/md0 -prp10000
}
