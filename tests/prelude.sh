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

# virtio-scsi-pci seems to be buggy: reading the superblock on the root
# filesystem randomly returns zeroes. Still true as of 2026-08-19 - the guest
# panics with "unable to mount root fs on /dev/sda" a few percent of boots,
# more often under load, with the disk itself enumerated and attached. It was
# briefly the default here and had to be backed out.
#
# Tests that need to unplug a device set it for themselves: qemu won't hotplug
# on the root complex, which is where virtio-blk disks are attached. Paying
# for that per-test costs one flaky test; paying for it globally costs a
# random few percent of every boot in the suite.
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
    # The root disk is virtio-blk on every bus (see libktest.sh qemu_disk), so
    # it only consumes a letter when the scratch devices are virtio-blk too.
    # On any other bus the first scratch device is sda, not sdb.
    local chars
    case $ktest_storage_bus in
	virtio-blk)	chars=( {b..z} ) ;;
	*)		chars=( {a..z} ) ;;
    esac

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

# qemu_monitor <command>...
#
# Run a qemu monitor command against the VM we are running inside. The monitor
# socket is on the host, so we ask the supervisor to do it, over the console,
# the same way set_watchdog asks for a timeout.
#
# Mostly for making devices come and go:
#
#	qemu_monitor device_del dev3
#	qemu_monitor device_add virtio-blk-pci,drive=disk3,id=dev3
#
# The device ids are dev$N, numbered in the order start_vm attaches them, and
# match the vm/dev-$N backing files.
#
# Fire and forget - there is no reply, by design. To know a command took
# effect, watch for it in the guest (/sys/block, or a udev event); that is a
# stronger check than the monitor's acknowledgement, and it is what a test
# actually cares about. Nothing arrives if the supervisor could not reach the
# socket, so a test must never treat "I asked" as "it happened".
#
# scratch_dev_unplug()/scratch_dev_plug() below are that done properly for
# scratch devices, and are what a test wants instead of this.
qemu_monitor()
{
    echo QEMU_MONITOR "$@"
}

# What qemu currently calls a scratch device, and whether the guest has it.
#
# A plug cannot reuse the ids the previous one had (see scratch_dev_plug), so
# this is what says which ids the device answers to now. It lives in a file
# rather than a variable because run_tests() runs each subtest in a subshell:
# a variable set by a test that unplugs and replugs dies with that subtest,
# and the next one would go on to delete an id qemu no longer has.
#
# Format is "gone|present <device id> <generation>".
scratch_dev_state_file()	{ echo "$ktest_tmp/qemu-scratch-dev.$1"; }

scratch_dev_state()
{
    local f=$(scratch_dev_state_file $1)

    if [[ -e $f ]]; then
	cat "$f"
    else
	echo "present dev$((ktest_scratch_dev_base + $1)) 0"
    fi
}

# After a failed test, put back whatever it unplugged: a test that dies between
# the unplug and the plug would otherwise take every test after it down with
# it, and five failures that are one failure are worse than useless.
scratch_dev_replug_all()
{
    local f state id gen

    for f in "${ktest_tmp:-/nonexistent}"/qemu-scratch-dev.*; do
	[[ -e $f ]] || continue

	read -r state id gen < "$f"
	[[ $state = gone ]] || continue

	echo "restoring scratch device ${f##*.}, left unplugged by a failed test"
	scratch_dev_plug "${f##*.}"
    done
}

# scratch_dev_unplug <index>
# scratch_dev_plug   <index>
#
# Take a scratch device away from the guest and give it back: the block device
# disappears entirely, along with anything udev built on top of it, the way it
# does when a disk is pulled and later reseated.
#
# The two things needed to name a device to qemu both come from start_vm, so
# nothing here has to be kept in step with how disks are attached there:
# $ktest_scratch_dev_base is the disk number of scratch device 0, and field N
# of $ktest_qemu_devices is the -device argument that created disk N.
#
# Waiting for the guest to catch up is the whole point, not a nicety. qemu
# discards a monitor command naming a device that doesn't exist and tells
# nobody, and qemu_monitor has no reply to check, so without the wait a test
# that named the wrong device would pass exactly as happily as one that
# worked.
scratch_dev_unplug()
{
    local dev=${ktest_scratch_dev[$1]}

    # Guarded rather than defaulted: an unset base would resolve to disk 0,
    # which is the root disk. Spelled :- so the guard is what reports it,
    # rather than nounset firing first with a message about a variable name.
    if [[ -z ${ktest_scratch_dev_base:-} ]]; then
	echo "scratch_dev_unplug: ktest_scratch_dev_base unset, don't know which qemu disk $dev is"
	exit 1
    fi

    if [[ ! -b $dev ]]; then
	echo "scratch_dev_unplug: $dev is not there to unplug"
	exit 1
    fi

    # Whichever hotplug driver claimed the slots registers them under
    # /sys/bus/pci/slots - acpiphp for the pcie-pci-bridge the scratch disks
    # sit behind, pciehp for a bare root port. Nothing there means the kernel
    # has no PCI hotplug at all, so qemu's eject request has no one to answer
    # it: say that outright rather than spending 30s and then reporting a
    # device that didn't move.
    local slots=(/sys/bus/pci/slots/*)

    if [[ ! -e ${slots[0]} ]]; then
	echo "scratch_dev_unplug: no PCI hotplug slots registered - is CONFIG_HOTPLUG_PCI_ACPI set?"
	exit 1
    fi

    local state id gen

    read -r state id gen <<< "$(scratch_dev_state $1)"

    qemu_monitor device_del $id
    wait_for_dev "$dev" gone "device_del $id"

    echo "gone $id $gen" > "$(scratch_dev_state_file $1)"
}

scratch_dev_plug()
{
    local dev=${ktest_scratch_dev[$1]}
    local devices drives

    if [[ -z ${ktest_scratch_dev_base:-} ]]; then
	echo "scratch_dev_plug: ktest_scratch_dev_base unset, don't know which qemu disk $dev is"
	exit 1
    fi

    mapfile -t devices <<< "${ktest_qemu_devices:-}"
    mapfile -t drives  <<< "${ktest_qemu_drives:-}"

    local nr=$((ktest_scratch_dev_base + $1))
    local spec=${devices[$nr]:-}
    local drive=${drives[$nr]:-}

    if [[ -z $spec || -z $drive ]]; then
	echo "scratch_dev_plug: no qemu drive/device for disk $nr"
	exit 1
    fi

    # The unplug took the backend with it - qdev's property release runs
    # blockdev_auto_del() - so the drive has to be put back before the device
    # can reference it. What it must NOT be put back as is the id it had
    # before: after a device_del, qemu can be left holding the id without the
    # backend, and answers both halves of that state at once -
    #
    #	drive_add  0 ...,id=disk1,...	Duplicate ID 'disk1' for drive
    #	device_add ...,drive=disk1	Property 'virtio-blk-device.drive'
    #					can't find value 'disk1'
    #
    # - the name taken, and the thing it names gone. The -drive on the command
    # line registers a QemuOpts entry under that id which outlives the
    # BlockBackend it created.
    #
    # drive_del cannot clean it up: it finds the drive by name, and the name is
    # what has already gone ("Error: Device 'disk1' not found", measured
    # against qemu 11.0.1). So don't try to reclaim the id - take a fresh one
    # for both the backend and the device, and rewrite the specs to match.
    # Nothing outside these two functions refers to either id.
    local state old_id gen

    read -r state old_id gen <<< "$(scratch_dev_state $1)"
    gen=$((gen + 1))

    drive=${drive/id=disk$nr,/id=disk${nr}p$gen,}
    spec=${spec/drive=disk$nr,/drive=disk${nr}p$gen,}
    spec=${spec/id=dev$nr/id=dev${nr}p$gen}

    qemu_monitor drive_add 0 "$drive"
    qemu_monitor device_add "$spec"

    echo "present dev${nr}p$gen $gen" > "$(scratch_dev_state_file $1)"

    wait_for_dev "$dev" present "device_add $spec"
}

# wait_for_dev <dev> gone|present <what we asked qemu for>
#
# The third argument is only for the failure message: what we asked for is the
# interesting thing by then, and the caller no longer has it in the log.
wait_for_dev()
{
    local dev=$1
    local want=$2
    local asked=$3
    local have
    local i

    for ((i = 0; i < 300; i++)); do
	have=gone
	if [[ -b $dev ]]; then
	    have=present
	fi

	if [[ $have = $want ]]; then
	    return 0
	fi

	sleep 0.1
    done

    echo "$dev not $want 30s after asking qemu for '$asked'"
    cat /proc/partitions
    exit 1
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

# ktest_skip <reason>
#
# Declare the current subtest not applicable here, and stop it: the runner
# reports NOTRUN rather than FAILED, and the run as a whole still succeeds.
#
# For a precondition the test cannot satisfy and that says nothing about the
# code under test - a kernel built without the feature, hardware that isn't
# there. NOT for "the thing I was testing didn't work". Dressing a failure as
# a skip is how a suite goes quietly green while the thing it guards rots, so
# the bar is: would a developer seeing this need to do something? Then it's a
# failure.
#
# The reason travels on the NOTRUN line, so it reaches the per-subtest status
# file that the dashboard shows - a skip whose reason you have to go digging
# in a log for is most of the way to no signal at all.
#
# Via a file because run_tests runs each subtest in a subshell, and an exit
# status can't carry a reason - or be told apart from a test that happened to
# exit with the same number.
ktest_skip()
{
    get_tmpdir

    echo "SKIPPING: $*"
    echo "$*" > "$ktest_tmp/ktest_skip"
    exit 1
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
    local tests_skipped=()

    # so ktest_skip's marker lands somewhere this shell can find it, and not
    # in a tmpdir the subshell made for itself
    get_tmpdir

    echo
    echo "Running tests $@"
    echo

    for i in $@; do
	echo "========= TEST   $i" | to_kmsg

	rm -f "$ktest_tmp/ktest_skip"

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
	elif [[ -e $ktest_tmp/ktest_skip ]]; then
	    echo "========= NOTRUN $i in $(($finish - $start))s: $(cat $ktest_tmp/ktest_skip)" | to_kmsg
	    tests_skipped+=($i)
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

	    scratch_dev_replug_all
	fi
    done

    echo
    echo "Passed: ${tests_passed[@]}"
    echo "Failed: ${tests_failed[@]}"
    if [[ ${#tests_skipped[@]} != 0 ]]; then
	echo "Skipped: ${tests_skipped[@]}"
    fi

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
