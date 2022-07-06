
parse_test_deps()
{
    ktest_cpus=$(nproc)
    ktest_mem=""
    ktest_timeout=""
    ktest_kernel_append=()
    ktest_images=()
    ktest_scratch_devs=()
    ktest_make_install=()
    ktest_kernel_config_require=()
    ktest_qemu_append=()

    local NEXT_SCRATCH_DEV="b"
    local TESTPROG=$1
    local BUILD_ON_HOST=""

    require-lib()
    {
	local req="$1"

	pushd "$(dirname "$req")"	> /dev/null
	. $(basename "$req")
	popd				> /dev/null
    }

    require-git()
    {
	local req="$1"
	local dir=$(basename $req)
	dir=${dir%%.git}

	if [[ $# -ge 2 ]]; then
	    dir=$2
	fi

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
	if [[ ! -d "$1" ]]; then
	    echo "require-make: $1 not found"
	    exit 1
	fi

	local req=$(readlink -e "$1")

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
	ktest_kernel_append+=($1)
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
	parse_arch "$1"
	checkdep_arch
    }

    pushd "$(dirname "$TESTPROG")"	> /dev/null
    . $(basename "$TESTPROG")
    popd				> /dev/null

    if [ -z "$ktest_mem" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$ktest_timeout" ]; then
	ktest_timeout=6000
    fi

    # may be overridden by test:
    if [[ $(type -t run_test) != function ]]; then
	run_test()
	{
	    local test=test_$1

	    if [[ $(type -t $test) != function ]]; then
		echo "test $1 does not exist"
		exit 1
	    fi

	    $test
	}
    fi

    # may be overridden by test:
    if [[ $(type -t run_tests) != function ]]; then
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

		    [[ $ktest_failfast = 1 ]] && break
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
    fi

    # may be overridden by test:
    if [[ $(type -t list_tests) != function ]]; then
	list_tests()
	{
	    declare -F|sed -ne '/ test_/ s/.*test_// p'
	}
    fi

    ktest_tests=$(list_tests)

    if [[ -z $ktest_tests ]]; then
	echo "No tests found"
	echo "TEST FAILED"
	exit 1
    fi

    local t

    # Ensure specified tests exist:
    if [[ -n $ktest_testargs ]]; then
	for t in $ktest_testargs; do
	    if ! echo "$ktest_tests"|grep -wq "$t"; then
		echo "Test $t not found"
		exit 1
	    fi
	done

	ktest_tests="$ktest_testargs"
    fi

    # Mark tests not run:
    local testname=$(basename -s .ktest "$ktest_test")
    mkdir -p "$ktest_out/out"
    for t in $ktest_tests; do
	t=$(echo "$t"|tr / .)

	mkdir -p $ktest_out/out/$testname.$t
	echo "========= NOT STARTED" > $ktest_out/out/$testname.$t/status
    done
}
