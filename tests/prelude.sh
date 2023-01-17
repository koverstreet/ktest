#!/usr/bin/env bash
# Basic libs for ktest tests:

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/../lib/common.sh

if [[ ! -v ktest_verbose ]]; then
    ktest_verbose=0
    ktest_priority=0
    ktest_cpus=$(nproc)
    ktest_mem=""
    ktest_timeout=""
    ktest_kernel_append=()

    # virtio-scsi-pci semes to be buggy: reading the superblock on the root
    # filesystem randomly returns zeroes
    #ktest_storage_bus=virtio-scsi-pci
    ktest_storage_bus=ahci

    ktest_images=()
    ktest_scratch_devs=()
    ktest_make_install=()
    ktest_kernel_config_require=()
    ktest_qemu_append=()

    NEXT_SCRATCH_DEV="b"
    BUILD_ON_HOST=""
fi

require-git()
{
    local req="$1"
    local dir=$(basename $req)
    dir=${dir%%.git}

    if [[ $# -ge 2 ]]; then
	dir=$2
    fi

    dir=$(dirname $(readlink -e "${BASH_SOURCE[1]}"))/$dir

    if [[ ! -d $dir ]]; then
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

require-qemu-append()
{
    local OLDIFS=$IFS
    IFS=','

    for i in $1; do
	ktest_kernel_config_require+=("$i")
    done

    IFS=$OLDIFS
}

require-kernel-append()
{
    ktest_kernel_append+=("$1")
}

config-scratch-devs()
{
    ktest_scratch_devs+=("$1")
}

config-pmem-devs()
{
    ktest_pmem_devs+=("$1")
}

config-image()
{
    ktest_images+=("$1")
}

config-cpus()
{
    ktest_cpus=$1
}

config-mem()
{
    ktest_mem=$1
}

config-timeout()
{
    n=$1
    if [ "${EXTENDED_DEBUG:-0}" == 1 ]; then
	n=$((n * 2))
    fi
    ktest_timeout=$n
}

config-arch()
{
    ktest_arch=$1
}

set_watchdog()
{
    echo WATCHDOG $1
}

run_test()
{
    local test=test_$1

    if [[ $(type -t $test) != function ]]; then
	echo "test $1 does not exist"
	exit 1
    fi

    $test
}

run_tests()
{
    local tests_passed=()
    local tests_failed=()

    echo
    echo "Running tests $@"
    echo

    for i in $@; do
	echo "========= TEST   $i"
	echo

	local start=$(date '+%s')
	local ret=0
	(set -e; run_test $i)
	ret=$?
	local finish=$(date '+%s')

	pkill -P $$ >/dev/null || true

	# XXX: check dmesg for warnings, oopses, slab corruption, etc. before
	# signaling success

	echo

	if [[ $ret = 0 ]]; then
	    echo "========= PASSED $i in $(($finish - $start))s"
	    tests_passed+=($i)
	else
	    echo "========= FAILED $i in $(($finish - $start))s"
	    tests_failed+=($i)

	    # Try to clean up after a failed test so we can run the rest of
	    # the tests - unless failfast is enabled, or there was only one
	    # test to run:

	    $ktest_failfast  && break
	    [[ $# = 1 ]] && break

	    for mnt in $(awk '{print $2}' /proc/mounts|grep ^/mnt|sort -r); do
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

main()
{
    if [[ $BASH_ARGC = 0 ]]; then
	exit 0
    fi

    local arg=$1
    shift

    case $arg in
	deps)
	    echo "ktest_arch=$ktest_arch"
	    echo "ktest_cpus=$ktest_cpus"
	    echo "ktest_mem=$ktest_mem"
	    echo "ktest_timeout=$ktest_timeout"
	    echo "ktest_kernel_append=(${ktest_kernel_append[@]})"
	    echo "ktest_storage_bus=$ktest_storage_bus"
	    echo "ktest_images=(${ktest_images[@]})"
	    echo "ktest_scratch_devs=(${ktest_scratch_devs[@]})"
	    echo "ktest_make_install=(${ktest_make_install[@]})"
	    echo "ktest_kernel_config_require=(${ktest_kernel_config_require[@]})"
	    echo "ktest_qemu_append=(${ktest_qemu_append[@]})"
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
