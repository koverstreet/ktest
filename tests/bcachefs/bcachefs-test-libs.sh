#!/bin/bash
#
# Library with some functions for writing bcachefs tests using the
# ktest framework.
#

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/../test-libs.sh

require-git http://evilpiepirate.org/git/bcachefs-tools.git
require-make bcachefs-tools

require-kernel-config BCACHEFS_FS

if [[ ! -v NO_BCACHEFS_DEBUG ]]; then
    require-kernel-config BCACHEFS_DEBUG
fi

require-kernel-config TRANSPARENT_HUGEPAGE

if [[ $ktest_arch = x86_64 ]]; then
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
	find /sys/fs/bcachefs -name prune_cache|{
	    while read f; do
		echo 1000000 > $f > /dev/null 2>&1 || true
	    done
	}

	sleep 5
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
	sleep 10
	echo 1 | tee /sys/fs/bcachefs/*/internal/trigger_gc >& /dev/null || true
    done
}

antagonist_switch_str_hash()
{
    cd /sys/fs/bcachefs

    while true; do
	for i in crc32c crc64 siphash; do
	    echo $i | tee */options/str_hash >& /dev/null || true
	    sleep 2
	done
    done
}

antagonist_switch_crc()
{
    cd /sys/fs/bcachefs

    while true; do
	for i in crc32c crc64; do
	    echo $i | tee */options/data_checksum */options/metadata_checksum >& /dev/null || true
	    sleep 2
	done
    done
}

antagonist_cat_sysfs_debugfs()
{
    set +o errexit
    set +o pipefail

    while true; do
	cd /sys/fs/bcachefs
	cat `find -type f` &> /dev/null || true

	cd /sys/kernel/debug/bcachefs
	cat `ls */* 2>/dev/null` &> /dev/null || true

	sleep 5
    done
}

bcachefs_antagonist()
{
    # Enable all bcachefs tracepoints - good for test coverage
    setup_tracing 'bcachefs:*'

    # Or alternately, only enable events check_counters will want to dump:
    #local ev=/sys/kernel/tracing/events/bcachefs/
    #echo 1|tee "$ev"/*fail*/enable "$ev"/*restart*/enable "$ev"/*blocked*/enable

    #echo 1 > /sys/module/bcachefs/parameters/expensive_debug_checks
    #echo 1 > /sys/module/bcachefs/parameters/debug_check_iterators
    #echo 1 > /sys/module/bcachefs/parameters/debug_check_btree_accounting
    #echo 1 > /sys/module/bcachefs/parameters/test_alloc_startup
    #echo 1 > /sys/module/bcachefs/parameters/test_restart_gc
    #echo 1 > /sys/module/bcachefs/parameters/test_reconstruct_alloc
    #echo 1 > /sys/module/bcachefs/parameters/verify_btree_ondisk
    #echo 1 > /sys/module/bcachefs/parameters/verify_all_btree_replicas
    #echo 1 > /sys/module/bcachefs/parameters/btree_gc_coalesce_disabled
    #echo 1 > /sys/module/bcachefs/parameters/key_merging_disabled
    #echo 1 > /sys/module/bcachefs/parameters/journal_seq_verify

    #enable_race_faults

    antagonist_expensive_debug_checks &
    antagonist_shrink &
    antagonist_sync &
    antagonist_trigger_gc &
    antagonist_cat_sysfs_debugfs &
    #antagonist_switch_str_hash &
}

check_counters()
{
    local dev=$1
    local nr_commits=$(bcachefs show-super -f counters "$dev"|awk '/transaction_commit/ {print $2}')
    local ratio=10
    local ret=0

    [[ $# -ge 2 ]] && ratio=$2

    local max_fail=$((nr_commits / ratio))

    local counters=$(bcachefs show-super -f counters "$dev"|grep -E '(fail|restart|blocked)'|grep -v path_relock_fail)

    while IFS= read -r line; do
	linea=($line)

	local event="${linea[0]}"
	local nr="${linea[1]}"

	if (( nr > max_fail )); then
	    echo "Too many $event: $nr"
	    # Insert 0 byte seperators at the beginning of each trace event,
	    # then grep in null separator mode to print full output of
	    # multiline trace events:
	    sed -e '/ \[[0-9]\{3\}\]/ i\\x00' /sys/kernel/tracing/trace|grep -z "$event"|head -n500
	    ret=1
	fi
    done <<< "$counters"

    if [[ $ret = 1 ]]; then
	echo "Max failed events:   $max_fail"
	echo "Transaction commits: $nr_commits"
    fi

    return $ret
}

fill_device()
{
    local filename=$1

    fio						\
	--filename="$filename"			\
	--ioengine=sync				\
	--name=write				\
	--rw=write				\
	--bs=16M				\
	--fill_fs=1
    echo 3 > /proc/sys/vm/drop_caches
}

run_fio_base()
{
    fio --eta=always				\
	--exitall_on_error=1			\
	--randrepeat=0				\
	--ioengine=libaio			\
	--iodepth=64				\
	--iodepth_batch=16			\
	--direct=1				\
	--numjobs=1				\
	--verify_fatal=1			\
	--filename=/mnt/fiotest		    	\
	"$@"
}

run_fio()
{
    local loops=$(((ktest_priority + 1) * 4))

    fio --eta=always				\
	--exitall_on_error=1			\
	--ioengine=libaio			\
	--iodepth=64				\
	--iodepth_batch=16			\
	--direct=1				\
	--numjobs=1				\
	--verify=meta				\
	--verify_fatal=1			\
	--buffer_compress_percentage=30		\
	--filename=/mnt/fiotest		    	\
	--filesize=3500M			\
	--loops=$loops				\
	"$@"
}

run_fio_randrw()
{
    set_watchdog 1200
    run_fio					\
	--name=randrw				\
	--rw=randrw				\
	--bsrange=4k-1M				\
	"$@"
}

run_basic_fio_test_counter_threshold()
{
    set_watchdog 1200
    local devs=()

    local ratio=$1
    shift

    for i in "$@"; do
	[[ ${i:0:1} != - ]] && devs+=($i)
    done

    bcachefs_antagonist

    run_quiet "" bcachefs format -f --discard --no_initialize "$@"

    mount -t bcachefs -o fsck "$(join_by : "${devs[@]}")" /mnt

    #enable_memory_faults
    run_fio_randrw
    #dd if=/dev/zero of=/mnt/foo bs=2M count=1024 oflag=direct
    #disable_memory_faults

    umount /mnt

    # test remount:
    #mount -t bcachefs -o fsck $(join_by : "${devs[@]}") /mnt
    #umount /mnt

    bcachefs fsck -ny "${devs[@]}"
    check_counters "${devs[0]}" "$ratio"
}

run_basic_fio_test()
{
    run_basic_fio_test_counter_threshold 10 "$@"
}

require-kernel-config DEBUG_FS
#require-kernel-config DYNAMIC_FAULT

run_fault_injection_test()
{
    local class="class $1"
    local fn=$2

    local control=/sys/kernel/debug/dynamic_fault/control
    local nr=$(grep "class:$1" $control|wc -l)

    for ((i=0; i<nr; i++)); do
	local fault="class $1 index $i"
	#echo -n "TESTING FAULT "; grep $class $control|sed -n $((i+1))p

	echo "TESTING FAULT $fault"

	set_faults "$fault enable"

	$fn "$fault"
	set_faults "$fault disable"
    done
}
