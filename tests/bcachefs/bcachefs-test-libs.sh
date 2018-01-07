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

antagonist_shrink()
{
    while true; do
	for file in $(find /sys/fs/bcachefs -name prune_cache); do
	    echo 100000 > $file > /dev/null 2>&1 || true

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

antagonist_switch_str_hash()
{
    cd /sys/fs/bcachefs

    while true; do
	for i in crc32c crc64 siphash; do
	    echo $i | tee */options/str_hash > /dev/null 2>&1 || true
	    sleep 2
	done
    done
}

antagonist_switch_crc()
{
    cd /sys/fs/bcachefs

    while true; do
	for i in crc32c crc64; do
	    echo $i | tee */options/data_checksum */options/metadata_checksum > /dev/null 2>&1 || true
	    sleep 2
	done
    done
}

bcachefs_antagonist()
{
    setup_tracing 'bcachefs:*'
    #echo 1 > /sys/module/bcachefs/parameters/expensive_debug_checks
    #echo 1 > /sys/module/bcachefs/parameters/verify_btree_ondisk
    #echo 1 > /sys/module/bcachefs/parameters/debug_check_bkeys
    #echo 1 > /sys/module/bcachefs/parameters/btree_gc_coalesce_disabled
    #echo 1 > /sys/module/bcachefs/parameters/key_merging_disabled

    enable_race_faults

    antagonist_expensive_debug_checks &
    antagonist_shrink &
    antagonist_sync &
    antagonist_trigger_gc &
    antagonist_switch_str_hash &
}

run_fio_base()
{
    fio --eta=always				\
	--randrepeat=0				\
	--ioengine=libaio			\
	--iodepth=64				\
	--iodepth_batch=16			\
	--direct=1				\
	--numjobs=1				\
	--verify=meta				\
	--verify_fatal=1			\
	--filename=/mnt/fiotest		    	\
	"$@"
}

run_fio()
{
    loops=$((($ktest_priority + 1) * 4))

    fio --eta=always				\
	--ioengine=libaio			\
	--iodepth=64				\
	--iodepth_batch=16			\
	--direct=1				\
	--numjobs=1				\
	--verify=meta				\
	--verify_fatal=1			\
	--buffer_compress_percentage=50		\
	--filename=/mnt/fiotest		    	\
	--filesize=3500M			\
	--loops=$loops				\
	"$@"
}

run_fio_randrw()
{
    run_fio					\
	--name=randrw				\
	--rw=randrw				\
	--bsrange=4k-1M				\
	"$@"
}

run_basic_fio_test()
{
    local devs=()

    for i in "$@"; do
	[[ ${i:0:1} != - ]] && devs+=($i)
    done

    bcachefs_antagonist

    if [[ $ktest_verbose = 1 ]]; then
	bcachefs format --error_action=panic "$@"
    else
	bcachefs format --error_action=panic "$@" >/dev/null
    fi
    mount -t bcachefs $(join_by : "${devs[@]}") /mnt

    #enable_memory_faults
    run_fio_randrw
    #disable_memory_faults

    umount /mnt

    # test remount:
    mount -t bcachefs $(join_by : "${devs[@]}") /mnt
    umount /mnt
}

require-kernel-config DEBUG_FS,DYNAMIC_FAULT
run_fault_injection_test()
{
    local class="class $1"
    local fn=$2

    local control=/sys/kernel/debug/dynamic_fault/control
    local nr=$(grep $class $control|wc -l)

    for ((i=0; i<nr; i++)); do
	echo -n "TESTING FAULT "; grep $class $control|sed -n $((i+1))p

	local fault="$class index $i"
	set_faults "$fault enable"

	$fn $fault
	set_faults "$fault disable"
    done
}
