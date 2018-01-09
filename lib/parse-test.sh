BUILD_DEPS=""

parse_test_deps()
{
    ktest_cpus="6"
    ktest_mem=""
    ktest_timeout=""
    ktest_kernel_append=""
    ktest_scratch_devs=()
    _KERNEL_CONFIG_REQUIRE=""
    
    TEST_RUNNING=""

    local NEXT_SCRATCH_DEV="b"
    local TESTPROG=$1
    local TESTDIR="$(dirname "$TESTPROG")"

    require-lib()
    {
	local req="$1"

	if [ "${req:0:1}" = "/" ]; then
	    local f="$req"
	else
	    local f="$TESTDIR/$req"
	fi

	local old="$TESTDIR"
	TESTDIR="$(dirname "$f")"
	. "$f"
	TESTDIR="$old"
    }

    # $1 is a source repository, which will be built (with make) and then turned
    # into a dpkg
    require-build-deb()
    {
	local req=$1
	local name=$(basename $req)
	local path=$(readlink -e "$TESTDIR/$req")

	[[ $BUILD_DEPS = 1 ]] || return 0

	checkdep debuild devscripts

	if ! [[ -d $path ]]; then
	    echo "build-deb dependency $req not found"
	    exit 1
	fi

	get_tmpdir
	local out="$ktest_tmp/out"

	pushd "$path"	> /dev/null

	echo -n "building $name... "

	if ! make > "$out" 2>$1 && [[ $? -eq 2 ]]; then
	    echo "Error building $req:"
	    cat "$out"
	    exit 1
	fi
	[[ $ktest_verbose = 1 ]] && cat "$out"

	popd		> /dev/null

	cp -drl $path $ktest_tmp
	pushd "$ktest_tmp/$name" > /dev/null

	# make -nc actually work:
	rm -f debian/*.debhelper.log

	if ! debuild --no-lintian -b -i -I -us -uc -nc > "$out" 2>$1; then
	    echo "Error creating package for $req: $?"
	    cat "$out"
	    exit 1
	fi

	echo done

	[[ $ktest_verbose = 1 ]] && cat "$out"

	popd		> /dev/null
    }

    require-kernel-config()
    {
	_KERNEL_CONFIG_REQUIRE+=",$1"
    }

    require-kernel-append()
    {
	ktest_kernel_append+=" $1"
    }

    config-scratch-devs()
    {
	ktest_scratch_devs+=("$1")
    }

    config-image()
    {
	ktest_image=$1
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

    . "$TESTPROG"

    if [ -z "$ktest_mem" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$ktest_timeout" ]; then
	echo "test must specify config-timeout"
	exit 1
    fi
}
