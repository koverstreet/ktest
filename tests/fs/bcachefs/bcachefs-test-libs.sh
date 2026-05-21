#!/bin/bash
#
# Library with some functions for writing bcachefs tests using the
# ktest framework.
#

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/../../test-libs.sh

# nodebug test variant: the harness passes ktest_bcachefs_no_debug (it
# rides testrunner's ktest_* passthrough). Translate it once to the
# internal NO_BCACHEFS_DEBUG that the checks below — and bcachefs_antagonist
# — key on, so those don't all need renaming. Must precede the
# NO_BCACHEFS_DEBUG block.
if [[ -v ktest_bcachefs_no_debug ]]; then
    export NO_BCACHEFS_DEBUG=1
fi

if [[ ! -v NO_BCACHEFS_DEBUG ]]; then
    require-kernel-config BCACHEFS_DEBUG
    require-kernel-config BCACHEFS_LOCK_TIME_STATS
    require-kernel-config BCACHEFS_NO_LATENCY_ACCT=n
    require-kernel-config BCACHEFS_TRANS_KMALLOC_TRACE
    # Plumb BCACHEFS_DEBUG into the DKMS module build: bcachefs's fs/Makefile
    # picks this up as a make var (inherited from env) and emits
    # -DCONFIG_BCACHEFS_DEBUG=1 for the module compile. The kernel config
    # requirements above only apply to in-tree builds; DKMS modules built
    # against upstream kernels (no bcachefs in-tree) need this separate
    # signal.
    export BCACHEFS_DEBUG=1
else
    require-kernel-config BCACHEFS_NO_LATENCY_ACCT=y
fi

if [[ -v ktest_bcachefs_inject_transaction_restarts ]]; then
    require-kernel-config BCACHEFS_INJECT_TRANSACTION_RESTARTS
    # ktest_bcachefs_inject_transaction_restarts is the harness transport
    # (it rides testrunner's ktest_* passthrough). bcachefs's fs/Makefile
    # reads the unprefixed BCACHEFS_INJECT_TRANSACTION_RESTARTS, so
    # re-export that for the DKMS build. Must precede require-git, which
    # triggers the build hook.
    export BCACHEFS_INJECT_TRANSACTION_RESTARTS=1
fi

# In-kernel unit tests (CONFIG_BCACHEFS_TESTS) — enabled for all bcachefs
# tests. require-kernel-config covers in-tree kernels; bcachefs's fs/Makefile
# reads the unprefixed BCACHEFS_TESTS for the DKMS build, so export that too.
# Must precede require-git, which triggers the build hook.
require-kernel-config BCACHEFS_TESTS
ktest_bcachefs_tests=1
export BCACHEFS_TESTS=1

require-git https://evilpiepirate.org/git/bcachefs-tools.git
# Cache key for the bcachefs DKMS module. The built .ko is fully
# determined by the kernel it compiles against (version + config), the
# bcachefs-tools revision, and the build flags — key on exactly that.
# An empty key (no git rev, a dirty tree, an unreadable kernel config)
# disables caching rather than risking a stale or wrong .ko.
dkms_cache_key() {
    local tools=$1
    local kver config rev

    kver=$(uname -r)
    config="/lib/modules/$kver/build/.config"
    if [[ ! -r $config ]]; then
        echo "dkms cache: disabled — kernel config unreadable ($config)" >&2
        return 0
    fi
    # The bcachefs-tools tree is bind-mounted from the host and owned by
    # a different uid than root in this VM — git rejects it as "dubious
    # ownership" and the checks below fatal out. Ephemeral build VM, so
    # opt out (safe.directory is honored only from global config, not -c).
    git config --system --add safe.directory '*' >&2

    if ! git -C "$tools" diff --quiet HEAD 2>/dev/null; then
        echo "dkms cache: disabled — $tools has uncommitted changes:" >&2
        git -C "$tools" status --short >&2
        return 0
    fi
    rev=$(git -C "$tools" rev-parse HEAD 2>/dev/null)
    if [[ -z $rev ]]; then
        echo "dkms cache: disabled — no git HEAD for $tools" >&2
        return 0
    fi

    # One hash over the scalar inputs (NUL-separated) followed by the
    # kernel config verbatim.
    {
        printf '%s\0' "$kver" "$rev" \
            "${BCACHEFS_DEBUG-}" "${BCACHEFS_TESTS-}" \
            "${BCACHEFS_INJECT_TRANSACTION_RESTARTS-}"
        cat "$config"
    } | sha256sum | cut -d' ' -f1
}

# Build + install bcachefs-tools and the bcachefs DKMS module. The module
# compile is by far the slowest part of test bringup, so it is cached
# host-side (keyed by dkms_cache_key) and shared across slots — the cache
# dir sits beside the per-slot workspaces. Write-once-per-key: concurrent
# misses for the same key both build and the first to rename in wins.
init_build_bcachefs_tools() {
    local jobs=$(( $(nproc) / 2 ))
    (( jobs < 1 )) && jobs=1
    local tools="$ktest_deps_dir/bcachefs-tools"

    make -j$jobs -C "$tools" PREFIX=/usr install

    if ! [[ -e /sys/fs/bcachefs ]]; then
	local kver=$(uname -r)
	local ko="/lib/modules/$kver/updates/dkms/bcachefs.ko"
	local key=$(dkms_cache_key "$tools")
	local cachedir=$(dirname "$ktest_deps_dir")/dkms-cache

	mkdir -p "$cachedir"
	local cache="$cachedir/$key"

	if [[ -n $key && -f "$cache/bcachefs.ko" ]]; then
	    echo "init_build_bcachefs_tools: dkms cache hit ($key)"
	    mkdir -p "$(dirname "$ko")"
	    cp "$cache/bcachefs.ko" "$ko"
	    depmod -a "$kver"
	    modprobe bcachefs
	    return
	fi

	# On DKMS build failure, surface the compiler output — otherwise the
	# test log just shows "Bad return status" with no reason.
	if ! make -j$jobs -C "$tools" PREFIX=/usr dkms-reload; then
	    echo "init_build_bcachefs_tools: DKMS build failed; dumping make.log" >&2
	    local log found=0
	    while IFS= read -r log; do
	        found=1
	        echo "===== $log =====" >&2
	        cat -- "$log" >&2
	    done < <(find -L /var/lib/dkms/bcachefs -name make.log -printf '%i\t%p\n' \
	                 2>/dev/null | sort -u -k1,1 | cut -f2-)
	    (( found )) || echo "init_build_bcachefs_tools: no make.log found" >&2
	    return 1
	fi

	# Populate the cache. Build into a temp dir and rename in: a concurrent
	# miss that lost the race just discards its copy (the .ko is already
	# installed locally either way).
	if [[ -n $key && -f $ko ]]; then
	    local tmp
	    mkdir -p "$cachedir"
	    if tmp=$(mktemp -d "$cachedir/.tmp.XXXXXX" 2>/dev/null); then
		if cp "$ko" "$tmp/bcachefs.ko" && mv -T "$tmp" "$cache" 2>/dev/null; then
		    echo "init_build_bcachefs_tools: dkms cache store ($key)"
		else
		    rm -rf "$tmp"
		fi
	    fi
	elif [[ -n $key ]]; then
	    echo "dkms cache: not stored — no module at $ko" >&2
	fi
    fi
}

require-kernel-config BCACHEFS_FS
require-kernel-config-soft BCACHEFS_ASYNC_OBJECT_LISTS
require-kernel-config UNICODE # casefolding

require-kernel-config TRANSPARENT_HUGEPAGE

if [[ $ktest_arch = x86_64 && ! ${ktest_kernel_config_require[*]} == *KMSAN* ]]; then
    require-kernel-config CRYPTO_CRC32C_INTEL
    require-kernel-config CRYPTO_POLY1305_X86_64
    require-kernel-config CRYPTO_CHACHA20_X86_64
fi

export BCACHEFS_KERNEL_ONLY=1

#Expensive:
#require-kernel-config CLOSURE_DEBUG

bcachefs_mem_in_use()
{
    echo 1 > /sys/module/rcutree/parameters/do_rcu_barrier

    # check_for_deadlock's allocations are module lifetime, not fs:

    # We get spurious leaks from readpage_bio_extend; the pagecache likes to
    # hold onto folios way longer than it should

    grep -v "0        0" /proc/allocinfo|
	grep fs/bcachefs/|
	grep -v "func:bch2_check_for_deadlock"|
	grep -v "func:readpage_bio_extend"
}

check_bcachefs_leaks()
{
    local iter=0
    while bcachefs_mem_in_use; do
	echo "mem in use: "

	if ((iter > 20)); then
	    bcachefs_mem_in_use
	    return 1
	fi
	((iter += 1))
	sleep 1
    done
}

check_bcachefs_errors()
{
    for i in $@; do
	if bcachefs show-super -f errors $i|
	    sed -n '/^errors /,${/^errors /!p;}'|
	    grep -E '[a-z]'; then
	    return 1
	fi
    done
}

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

expensive_debug_checks_set()
{
    files="expensive_debug_checks debug_check_btree_locking debug_check_iterators debug_check_bset_lookups debug_check_btree_accounting debug_check_bkey_unpack"
    echo $1 |tee $files >& /dev/null || true
}

antagonist_expensive_debug_checks()
{
    # This only exists if CONFIG_BCACHE_DEBUG is on
    cd /sys/module/bcachefs/parameters

    while true; do
	expensive_debug_checks_set 1
	sleep 5
	expensive_debug_checks_set 0
	sleep 10
    done
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

bcachefs_slowpath_event_filter()
{
    grep  -E '(fail|restart|blocked|full)'|
	grep -vE '(btree_path|mem_realloced|trans_restart_injected|io_move_write_fail|and_poison|write_buffer_flush|journal_res_get_blocked)'
}

bcachefs_slowpath_tracepoints()
{
    ls /sys/kernel/tracing/events/bcachefs|bcachefs_slowpath_event_filter
}

bcachefs_antagonist()
{
    # Enable tracepoints check_bcachefs_counters will want to see
    setup_tracing `bcachefs_slowpath_tracepoints`

    # Enable all bcachefs tracepoints - good for test coverage, but very heavy
    # setup_tracing bcachefs:*

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

    if [[ ! -v NO_BCACHEFS_DEBUG ]]; then
	antagonist_expensive_debug_checks &
	antagonist_shrink &
	antagonist_sync &
	antagonist_trigger_gc &
	antagonist_cat_sysfs_debugfs &
	#antagonist_switch_str_hash &
    fi
}

get_slowpath_counters()
{
    local dev=$1

    bcachefs show-super --field-only counters "$dev"|
	bcachefs_slowpath_event_filter|
	grep -v  ' 0$' || true
}

_check_bcachefs_counters()
{
    local dev=$1
    local nr_commits=$(bcachefs show-super --field-only counters "$dev"|awk '/\<transaction_commit\>/ {print $2}')
    local nr_data_update=$(bcachefs show-super --field-only counters "$dev"|awk '/\<data_update\>/ {print $2}')
    local ratio=20
    local ret=0

    [[ -z $nr_commits ]] && return 0

    [[ $# -ge 2 ]] && ratio=$2

    local counters=$(set +e; set +o pipefail; get_slowpath_counters $dev)

    [[ -z $counters ]] && return 0

    while IFS= read -r line; do
	linea=($line)

	local event="${linea[0]}"
	local nr="${linea[1]}"

	local max_fail=$((nr_commits / ratio))

	if echo $event|grep -q data_update; then
	    max_fail=$((nr_data_update / ratio))
	fi

	max_fail=$((max_fail + 100))

	if [[ $event = trans_restart_would_deadlock ]]; then
	    max_fail=$((max_fail * 5))
	fi

	if [[ $event = bucket_alloc_fail ]]; then
	    max_fail=$((max_fail * 5))
	fi

	if (( nr > max_fail )); then
	    echo "$dev: Too many $event: $nr (max: $max_fail)"
	    # Insert 0 byte seperators at the beginning of each trace event,
	    # then grep in null separator mode to print full output of
	    # multiline trace events:
	    sed -e '/ \[[0-9]\{3\}\]/ i\\x00' /sys/kernel/tracing/trace|grep -z "$event"|tail -n500 || true
	    ret=1
	fi
    done <<< "$counters"

    if [[ $ret = 1 ]]; then
	echo "Max failed events:   $max_fail"
	echo "Transaction commits: $nr_commits"
	echo "Data update sectors: $nr_data_update"
    fi

    # some fstests do strange things that will cause this to fail - we don't particularly care:
    bcachefs reset-counters $dev >& /dev/null || true
    return $ret
}

check_bcachefs_counters()
{
    for dev in $@; do
	_check_bcachefs_counters $dev
    done
}

bcachefs_test_end_checks()
{
    check_bcachefs_leaks
    check_bcachefs_errors $@
    check_bcachefs_counters $@
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
	--verify=sha1				\
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

    run_quiet "" bcachefs format -f --discard --no_initialize --errors=ro "$@"

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
    bcachefs_test_end_checks "${devs[0]}" "$ratio"
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
