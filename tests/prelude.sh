#!/usr/bin/env bash
# Basic libs for ktest tests:

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/../lib/common.sh

: "${ktest_deps_dir:=${HOME:-/root}/.ktest/deps}"

if [[ ! -v ktest_interactive ]]; then
    ktest_failfast=false
    ktest_interactive=false
    ktest_verbose=false
    ktest_priority=0
fi

# Scalar defaults: only set if not already provided (by the harness env
# or an eval'd `deps` output) — an inherited value should win. Guarded
# per-variable: the harness exports an arbitrary subset of ktest_* vars,
# so a single-variable proxy guard would skip the defaults for the rest.
: "${ktest_cpus:=$(nproc)}"
: "${ktest_mem:=}"
: "${ktest_timeout:=}"
: "${ktest_timeout_multiplier:=1}"
: "${ktest_mem_multiplier:=1}"

# virtio-scsi-pci semes to be buggy: reading the superblock on the root
# filesystem randomly returns zeroes
#ktest_storage_bus=virtio-scsi-pci
: "${ktest_storage_bus:=virtio-blk}"

: "${ktest_compiler:=${CC:-gcc}}"
: "${ktest_allow_taint:=false}"

: "${ktest_tests_unknown:=false}"
: "${ktest_kconfig_base:=}"
: "${ktest_no_kbuild:=false}"
: "${ktest_no_vm:=false}"
: "${ktest_kbuild_target:=}"

: "${BUILD_ON_HOST:=}"

# Accumulator arrays/counter: always start fresh. The test body appends
# to these (config-scratch-devs, require-kernel-config, …); bash arrays
# can't be inherited through the environment anyway, so there's nothing
# a guard could correctly preserve. Must stay outside the ktest_cpus
# guard — an inherited ktest_cpus there would skip them and leave
# config-* appending to unset vars.
ktest_kernel_append=()
ktest_kernel_make_append=()
ktest_images=()
ktest_rw_images=()
ktest_scratch_dev=()
ktest_scratch_dev_sizes=()
ktest_scratch_dev_count=0
ktest_make_install=()
ktest_kernel_config_require=()
ktest_kernel_config_require_soft=()
ktest_qemu_append=()

case $ktest_storage_bus in
    virtio-blk)
        ktest_dev_prefix="vd"
        ;;
    *)
        ktest_dev_prefix="sd"
        ;;
esac

require-git()
{
    local req="$1"
    local dir=$(basename $req)
    dir=${dir%%.git}

    # Second-arg relative paths (e.g. "../xfstests") collapse to their
    # basename — the deps dir is flat, with no hierarchy to step through.
    if [[ $# -ge 2 ]]; then
	dir=$(basename "$2")
    fi

    dir=$ktest_deps_dir/$dir

    if [[ ! -d $dir ]]; then
	mkdir -p "$ktest_deps_dir"
	git clone $req $dir
    fi
}

do-build-deb()
{
    local path=$(readlink -e "$1")
    local name=$(basename $path)

    get_tmpdir

    make -C "$path"

    cp -drl $path $ktest_tmp
    pushd "$ktest_tmp/$name" > /dev/null

    # make -nc actually work:
    rm -f debian/*.debhelper.log

    debuild --no-lintian -b -i -I -us -uc -nc
    popd > /dev/null
}

# $1 is a source repository, which will be built (with make) and then turned
# into a dpkg
require-build-deb()
{
    local req=$1

    if ! [[ -d $req ]]; then
	echo "build-deb dependency $req not found"
	exit 1
    fi

    checkdep debuild devscripts

    run_quiet "building $(basename $req)" do-build-deb $req
}

require-make()
{
    local req=$(dirname $(readlink -e ${BASH_SOURCE[1]}))/$1

    if [[ ! -d $req ]]; then
	echo "require-make: $req not found"
	exit 1
    fi

    ktest_make_install+=("$req")

    if [[ -n $BUILD_ON_HOST ]]; then
	run_quiet "building $1" make -C "$req"
    fi
}

require-kernel-config()
{
    local OLDIFS=$IFS
    IFS=','

    for i in $1; do
	ktest_kernel_config_require+=("$i")
    done

    IFS=$OLDIFS
}

require-kernel-config-soft()
{
    ktest_kernel_config_require_soft+=("$1")
}

# True if dotted-numeric version $1 >= $2 (sort -V ordering). An empty $1 — e.g.
# $ktest_kernel_version outside a kernel build — sorts below everything, so it
# compares as false.
version-ge()
{
    [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n1)" = "$1" ]]
}

require-qemu-append()
{
    ktest_qemu_append+=("$@")
}

require-qemu-prepend()
{
    ktest_qemu_prepend+=("$@")
}

require-kernel-append()
{
    ktest_kernel_append+=("$1")
}

require-kernel-make-append()
{
    ktest_kernel_make_append+=("$1")
}

require-gcov()
{
    local dir=$(echo "${1%/}"|tr / _)

    require-kernel-make-append "GCOV_PROFILE_$dir=y"
    require-kernel-config GCOV_KERNEL
}

config-scratch-devs()
{
    local chars=( {b..z} )

    ktest_scratch_dev+=("/dev/${ktest_dev_prefix}${chars[$ktest_scratch_dev_count]}")
    ktest_scratch_dev_count=$((ktest_scratch_dev_count + 1))

    ktest_scratch_dev_sizes+=("$1")
}

config-pmem-devs()
{
    ktest_pmem_devs+=("$1")
}

config-image()
{
    ktest_images+=("$1")
}

config-rw-image()
{
    ktest_rw_images+=("$1")
}

config-cpus()
{
    ktest_cpus=$1
}

config-mem()
{
    local bytes=$(echo $1|numfmt --from=iec)
    ktest_mem=$((bytes / 1048576))
}

config-mem-multiplier()
{
    ktest_mem_multiplier=$(($ktest_mem_multiplier * $1))
}

config-timeout()
{
    n=$1
    if [ "${EXTENDED_DEBUG:-0}" == 1 ]; then
	n=$((n * 2))
    fi
    ktest_timeout=$n
}

config-timeout-multiplier()
{
    ktest_timeout_multiplier=$(($ktest_timeout_multiplier * $1))
}

config-arch()
{
    ktest_arch=$1
}

config-compiler()
{
    ktest_compiler=$1
}

config-no-kbuild()
{
    ktest_no_kbuild=true
}

config-no-vm()
{
    ktest_no_vm=true
}

config-kconfig-base()
{
    ktest_kconfig_base=$1
}

config-kbuild-target()
{
    ktest_kbuild_target=$1
}

allow_taint()
{
    ktest_allow_taint=true
}

create_ktest_user()
{
    groupadd -g 1000 ktest_group	>& /dev/null || true
    useradd -u 1000 -g 1000 ktest_user	>& /dev/null || true
}

# set_as_user
#
# Sets $as_user to a command prefix that runs its arguments unprivileged, and
# $as_user_uid/$as_user_gid to the identity they'll run as.
#
# Tests that enforce a limit against a caller need this, because root is not an
# ordinary caller: it holds CAP_SYS_RESOURCE, which bcachefs' ignore_hardlimit()
# honours, so a quota set against root is never enforced against root. A test
# that sets a limit and then exceeds it as root is testing nothing.
#
# The uid is reported rather than assumed because the two implementations don't
# name the same thing - setpriv takes a number, runuser takes a name - and a
# caller that has to name the same identity somewhere else ("setquota -u") has
# to use whatever we actually ran as.
set_as_user()
{
    if command -v setpriv > /dev/null; then
	as_user_uid=65534
	as_user_gid=65534
	as_user="setpriv --reuid=$as_user_uid --regid=$as_user_gid --clear-groups"
    elif command -v runuser > /dev/null; then
	as_user_uid=$(id -u nobody)
	as_user_gid=$(id -g nobody)
	as_user="runuser -u nobody --"
    else
	echo "TEST FAILED: no setpriv or runuser to drop privileges with"
	exit 1
    fi
}

set_watchdog()
{
    ktest_timeout=$1
    ktest_timeout=$((ktest_timeout * ktest_timeout_multiplier))

    echo WATCHDOG $ktest_timeout
}

# assert_output_lacks PATTERN CMD...
# assert_output_has   PATTERN CMD...
#
# Assert on a command's output, having first established there IS output to
# assert on.
#
# The idiom these replace is
#
#	! some_command | grep -q PATTERN
#
# which is a silent pass waiting to happen: pipefail isn't set here, so only
# grep's status counts. If some_command segfaults, exits nonzero, or has its
# output format changed out from under the test, grep matches nothing, the
# negation turns that into success, and the assertion has quietly stopped
# asserting. (Setting pipefail makes it worse, not better: a failing command
# would then mask even a real match.)
#
# An assertion whose command died is not a passing assertion - it's a test that
# can no longer tell you what it claims to. Fail, and print what was captured.

# What the command actually said, for the assertions below. Labelled and
# indented because it lands in a log between kernel messages and ktest's own
# errors: unmarked, the one thing that says why the assertion failed reads as
# ambient noise, and the reader is left with what we expected and no sight of
# what we got.
print_output()
{
    echo "output was:"
    sed 's/^/\t/' <<<"$1"
}

assert_output_lacks()
{
    local pattern=$1 out rc=0
    shift

    # "|| rc=$?", not a bare assignment followed by rc=$?: run_test runs these
    # under `set -e`, where a bare `out=$(cmd)` with a failing cmd exits the
    # shell on the spot - before the check below, making the whole diagnostic
    # unreachable and reporting the raw prelude line instead. A compound
    # command is exempt from errexit, so this keeps the status to test.
    out=$("$@" 2>&1) || rc=$?
    if [[ $rc != 0 ]]; then
	echo "FAILED: '$*' exited $rc, so '$pattern must be absent' was never tested"
	print_output "$out"
	exit 1
    fi

    if grep -q -- "$pattern" <<<"$out"; then
	echo "FAILED: '$pattern' present in output of '$*'"
	print_output "$out"
	exit 1
    fi
}

assert_output_has()
{
    local pattern=$1 out rc=0
    shift

    # See assert_output_lacks() on "|| rc=$?" and errexit.
    out=$("$@" 2>&1) || rc=$?
    if [[ $rc != 0 ]]; then
	echo "FAILED: '$*' exited $rc, so '$pattern must be present' was never tested"
	print_output "$out"
	exit 1
    fi

    if ! grep -q -- "$pattern" <<<"$out"; then
	echo "FAILED: '$pattern' missing from output of '$*'"
	print_output "$out"
	exit 1
    fi
}

# assert_fails CMD...
#
# Assert the command failed. Use this instead of
#
#	! some_command
#
# which does nothing at all: bash's `set -e` explicitly exempts any command
# whose return value is inverted with '!', so a negated assertion cannot fail a
# test no matter what the command does. It reads like an assertion, it is in the
# test for the same reason an assertion would be, and it has never once fired.
#
#	set -e
#	! true				# keeps going
#	! echo x | grep -q x		# grep matches - and keeps going
#
# Prefer assert_fails_with when you can name something the failure should say;
# "it failed" and "it failed for the reason we meant" are different assertions,
# and a command can usually fail for reasons that have nothing to do with what
# the test is about.
# Unlike the three above this doesn't capture: only the exit status is being
# asserted on, so the command's output can go straight to the log where it's
# useful, and the command runs in this shell rather than a subshell - which
# matters when it's a function (run_fio) rather than a binary.
assert_fails()
{
    local rc=0

    # See assert_fails_with() on "|| rc=$?" and errexit.
    "$@" || rc=$?
    if [[ $rc == 0 ]]; then
	echo "FAILED: '$*' succeeded; expected it to fail"
	exit 1
    fi
}

# assert_fails_with PATTERN CMD...
#
# The error-path counterpart of the two above: assert the command failed AND
# said the right thing about it.
#
# Both halves matter, and the exit status is the half that's easy to lose. For
# a refusal - "fsck declined to touch a mounted filesystem" - success and
# failure can produce similar-looking output, and a test that only grepped
# would pass whether we refused or went ahead and rewrote a live filesystem.
# Check the status first, and print what was captured when it's wrong.
assert_fails_with()
{
    local pattern=$1 out rc=0
    shift

    # "|| rc=$?" is load-bearing here, not defensive: run_test runs under
    # `set -e`, and this helper exists to run commands that fail, so a bare
    # assignment would exit the shell on every single call.
    out=$("$@" 2>&1) || rc=$?
    if [[ $rc == 0 ]]; then
	echo "FAILED: '$*' succeeded; expected failure matching '$pattern'"
	print_output "$out"
	exit 1
    fi

    if ! grep -q -- "$pattern" <<<"$out"; then
	echo "FAILED: '$*' exited $rc, but '$pattern' missing from output"
	print_output "$out"
	exit 1
    fi
}

# Scan what the kernel logged during this test - everything after the marker
# run_tests() wrote to kmsg before calling us.
#
# If the marker isn't there, the ring buffer overflowed and evicted it. It is
# FIFO, so everything older than the marker went first: whatever is left is all
# this test's own output, and we scan the lot. The previous version required a
# marker and printed nothing without one, so an overflowing test was checked
# against an empty input and passed - the noisiest tests, the ones most likely
# to have tripped something, were exactly the ones that stopped being checked.
#
# Scanning everything is still best-effort, since a BUG early enough in the test
# would have been evicted too, so say so rather than let a quiet pass stand in
# for a clean one.
check_dmesg()
{
    ! dmesg |
	awk '
	    { line[NR] = $0 }
	    /========= TEST/ { last = NR }
	    END {
		if (!last)
		    print "ktest: dmesg ring buffer overflowed - the TEST marker is gone, so anything logged earlier in this test is lost and this check is best-effort" > "/dev/stderr"
		for (i = last + 1; i <= NR; i++)
		    print line[i]
	    }' |
	grep -E -q -e "kernel BUG at" \
	    -e "WARNING:" \
	    -e "\bBUG:" \
	    -e "Oops:" \
	    -e "possible recursive locking detected" \
	    -e "Internal error" \
	    -e "(INFO|ERR): suspicious RCU usage" \
	    -e "INFO: possible circular locking dependency detected" \
	    -e "general protection fault:" \
	    -e "BUG .* remaining" \
	    -e "UBSAN:"
}

ktest_in_vm()
{
    [[ -e /dev/kmsg ]] && [[ -w /dev/kmsg ]]
}

to_kmsg()
{
    if ktest_in_vm; then
	cat > /dev/kmsg
    else
	cat
    fi
}

run_test()
{
    local test_file=$(basename -s .ktest $0)
    local test_name=$1
    local test_fn=test_$test_name
    local out_base=${ktest_out:-/ktest-out}
    local test_output=$out_base/out/$test_file.$test_name

    if [[ $(type -t $test_fn) != function ]]; then
	echo "test $test_name does not exist"
	exit 1
    fi

    mkdir -p $test_output

    if ktest_in_vm; then
	echo "|/bin/cp --sparse=always /dev/stdin $test_output/core.%e.PID%p.SIG%s.TIME%t" > /proc/sys/kernel/core_pattern
    fi

    $test_fn

    if ktest_in_vm; then
	check_dmesg
	dmesg -C
    fi
}

run_tests()
{
    local tests_passed=()
    local tests_failed=()

    echo
    echo "Running tests $@"
    echo

    for i in $@; do
	echo "========= TEST   $i" | to_kmsg

	local start=$(date '+%s')
	local ret=0
	(set -e; run_test $i)
	ret=$?
	local finish=$(date '+%s')

	pkill -P $$ >/dev/null || true

	echo

	if [[ $ret = 0 ]]; then
	    echo "========= PASSED $i in $(($finish - $start))s" | to_kmsg
	    tests_passed+=($i)
	else
	    echo "========= FAILED $i in $(($finish - $start))s" | to_kmsg
	    tests_failed+=($i)

	    # Try to clean up after a failed test so we can run the rest of
	    # the tests - unless failfast is enabled, or there was only one
	    # test to run:

	    $ktest_failfast  && break
	    [[ $# = 1 ]] && break

	    awk '{print $2}' /proc/mounts | grep ^/mnt | sort -r 2>/dev/null | while read -r mnt; do
		while [[ -n $(fuser -k -M -m $mnt) ]]; do
		    sleep 1
		done
		umount $mnt
	    done
	fi
    done

    echo
    echo "Passed: ${tests_passed[@]}"
    echo "Failed: ${tests_failed[@]}"

    return ${#tests_failed[@]}
}

list_tests()
{
    declare -F|sed -ne '/ test_/ s/.*test_// p'
}

# must have at least one init function to avoid errors below:
init_noop()
{
    true
}

run_init_hooks()
{
    for h in `declare -F|grep -Eo '\<init_.*'`; do
	echo "hook $h"
	$h
    done
}

main()
{
    if [[ $# = 0 ]]; then
	exit 0
    fi

    local arg=$1
    shift

    case $arg in
	deps)
	    echo "ktest_arch=$ktest_arch"
	    echo "ktest_compiler=$ktest_compiler"
	    echo "ktest_cpus=$ktest_cpus"
	    echo "ktest_mem=$((ktest_mem * ktest_mem_multiplier))"
	    echo "ktest_timeout=$((ktest_timeout * ktest_timeout_multiplier))"
	    echo "ktest_kernel_append=(${ktest_kernel_append[@]})"
	    echo "ktest_kernel_make_append=(${ktest_kernel_make_append[@]})"
	    echo "ktest_storage_bus=$ktest_storage_bus"
	    echo "ktest_images=(${ktest_images[@]})"
	    echo "ktest_rw_images=(${ktest_rw_images[@]})"
	    echo "ktest_scratch_dev_sizes=(${ktest_scratch_dev_sizes[@]})"
	    echo "ktest_make_install=(${ktest_make_install[@]})"
	    echo "ktest_kernel_config_require=(${ktest_kernel_config_require[@]})"
	    echo "ktest_kernel_config_require_soft=(${ktest_kernel_config_require_soft[@]})"
	    echo "ktest_qemu_append=(${ktest_qemu_append[@]})"
	    echo "ktest_qemu_prepend=(${ktest_qemu_prepend[@]})"
	    echo "ktest_allow_taint=$ktest_allow_taint"
	    echo "ktest_tests_unknown=$ktest_tests_unknown"
	    echo "ktest_kconfig_base=$ktest_kconfig_base"
	    echo "ktest_no_kbuild=$ktest_no_kbuild"
	    echo "ktest_no_vm=$ktest_no_vm"
	    echo "ktest_kbuild_target=$ktest_kbuild_target"
	    ;;
	init)
	    create_ktest_user
	    run_init_hooks
	    ;;
	list-tests)
	    list_tests
	    ;;
	run-tests)
	    run_tests "$@"
	    ;;
	*)
	    usage
	    exit 1
	    ;;
    esac
}
