#
# Library with some functions for writing block layer tests using the
# ktest framework.
#

require-lib prelude.sh

config-mem 2G

require-make ../ltp-fsx/Makefile ltp-fsx

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

#
# Set up a block device without bcache.
#
setup_blkdev() {
    DEVICES="/dev/sdb"
}

# Usage:
# setup_tracing tracepoint_glob
setup_tracing()
{
    echo > /sys/kernel/debug/tracing/trace
    echo 4 > /sys/kernel/debug/tracing/buffer_size_kb
    echo $1 > /sys/kernel/debug/tracing/set_event
    echo 1 > /proc/sys/kernel/ftrace_dump_on_oops
    echo 1 > /sys/kernel/debug/tracing/options/overwrite
    echo 1 > /sys/kernel/debug/tracing/tracing_on
}

dump_trace()
{
    cat /sys/kernel/debug/tracing/trace
}

#
# Mount file systems on all block devices.
#
existing_fs() {
    case $1 in
	ext4)
	    opts="-o errors=panic"
	    ;;
	xfs)
	    opts=""
	    ;;
	*)
	    opts=""
	    ;;
    esac

    for dev in $DEVICES; do
	mkdir -p /mnt/$dev
	mount $dev /mnt/$dev -t $1 $opts
    done
}

#
# Set up file systems on all block devices and mount them.
#
setup_fs()
{
    for dev in $DEVICES; do
	case $1 in
	    xfs)
		opts="-f"
		;;
	    ext4)
		opts="-F"
		;;
	    *)
		opts=""
		;;
	esac

	mkfs.$1 $opts $dev
    done
    existing_fs $1
}

stop_fs()
{
    for dev in $DEVICES; do
	umount /mnt/$dev
    done
}

# Block device workloads
#
# The DEVICES variable must be set to a list of devices before any of the
# below workloads are involed.

wait_all()
{
    for job in $(jobs -p); do
	wait $job
    done
}

run_bonnie()
{
    echo "=== start bonnie at $(date)"
    loops=$((($ktest_priority + 1) * 4))

    (
	for dev in $DEVICES; do
	    bonnie++ -x $loops -r 128 -u root -d /mnt/$dev &
	done

	wait_all
    )

    echo "=== done bonnie at $(date)"
}

run_dbench()
{
    echo "=== start dbench at $(date)"
    duration=$((($ktest_priority + 1) * 30))

    (
	for dev in $DEVICES; do
	    dbench -S -t $duration 2 -D /mnt/$dev &
	done

	wait_all
    )

    echo "=== done dbench at $(date)"
}

run_fio()
{
    echo "=== start fio at $(date)"
    loops=$(($ktest_priority / 2 + 1))

    (
	# Our default working directory (/cdrom) is not writable,
	# fio wants to write files when verify_dump is set, so
	# change to a different directory.
	cd $LOGDIR

	for dev in $DEVICES; do
	    fio --eta=always		\
		--randrepeat=0		\
		--ioengine=libaio	\
		--iodepth=64		\
		--iodepth_batch=16	\
		--direct=1		\
		--numjobs=1		\
		--buffer_compress_percentage=20\
		--verify=meta		\
		--verify_fatal=1	\
		--verify_dump=1		\
		--filename=$dev		\
		--fill_fs=1		\
					\
		--name=seqwrite		\
		--stonewall		\
		--rw=write		\
		--bsrange=4k-128k	\
		--loops=$loops		\
					\
		--name=randwrite	\
		--stonewall		\
		--rw=randwrite		\
		--bsrange=4k-128k	\
		--loops=$loops		\
					\
		--name=randwrite_small	\
		--stonewall		\
		--rw=randwrite		\
		--bs=4k			\
		--loops=$loops		\
					\
		--name=randread		\
		--stonewall		\
		--rw=randread		\
		--bs=4k			\
		--loops=$loops		&
	done

	wait_all
    )

    echo "=== done fio at $(date)"
}

run_fsx()
{
    echo "=== start fsx at $(date)"
    numops=$((($ktest_priority + 1) * 300000))

    (
	for dev in $DEVICES; do
	    ltp-fsx -N $numops /mnt/$dev/foo &
	done

	wait_all
    )

    echo "=== done fsx at $(date)"
}

# Bcache antagonists

enable_faults()
{
    f=/sys/kernel/debug/dynamic_fault/control

    if [[ -f $f ]]; then
	echo "class memory	frequency 100"	> $f
	echo "class race	frequency 100"	> $f
    fi
}

disable_faults()
{
    f=/sys/kernel/debug/dynamic_fault/control

    [[ -f $f ]] || return

    echo "class memory	disable"	> $f
    echo "class race	disable"	> $f
}

antagonist_sync()
{
    while true; do
	sync
	sleep 0.5
    done
}

antagonist_drop_caches()
{
    echo 4 > /proc/sys/vm/drop_caches

    while true; do
	echo 3 > /proc/sys/vm/drop_caches
	sleep 5
    done
}

stress_timeout()
{
    echo $((($ktest_priority + 3) * 600))
}

block_device_verify_dd()
{
    dd if=$1 of=/root/cmp bs=4096 count=1 iflag=direct
    cmp /root/cmp /root/orig
}

block_device_dd()
{
    dd if=/dev/urandom of=/root/orig bs=4096 count=1
    dd if=/root/orig of=$1 bs=4096 count=1 oflag=direct
    dd if=$1 of=/root/cmp bs=4096 count=1 iflag=direct
    cmp /root/cmp /root/orig

    dd if=/dev/urandom of=/root/orig bs=4096 count=1
    dd if=/root/orig of=$1 bs=4096 count=1 oflag=direct
}
