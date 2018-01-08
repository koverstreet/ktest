BUILD_DEPS=""

parse_test_deps()
{
    _CPUS="6"
    _MEM=""
    _TIMEOUT=""
    _KERNEL_CONFIG_REQUIRE=""
    _NR_VMS="1"
    _VMSTART_ARGS=(" ")
    TEST_RUNNING=""

    local NEXT_SCRATCH_DEV="b"
    local TESTPROG=$1
    local TESTDIR="$(dirname "$TESTPROG")"

    ktest_priority=$PRIORITY

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
	local out="$TMPDIR/out"

	pushd "$path"	> /dev/null

	echo -n "building $name... "

	if ! make > "$out" 2>$1 && [[ $? -eq 2 ]]; then
	    echo "Error building $req:"
	    cat "$out"
	    exit 1
	fi
	[[ $VERBOSE = 1 ]] && cat "$out"

	popd		> /dev/null

	cp -drl $path $TMPDIR
	pushd "$TMPDIR/$name" > /dev/null

	# make -nc actually work:
	rm -f debian/*.debhelper.log

	if ! debuild --no-lintian -b -i -I -us -uc -nc > "$out" 2>$1; then
	    echo "Error creating package for $req: $?"
	    cat "$out"
	    exit 1
	fi

	echo done

	[[ $VERBOSE = 1 ]] && cat "$out"

	popd		> /dev/null
    }

    require-kernel-config()
    {
	_KERNEL_CONFIG_REQUIRE+=",$1"
    }

    require-kernel-append()
    {
	_VMSTART_ARGS+=(--append="$1")
    }

    config-scratch-devs()
    {
	_VMSTART_ARGS+=(--scratchdev="$1")
    }

    config-image()
    {
	_VMSTART_ARGS+=(--image="$1")
    }

    config-cpus()
    {
	_CPUS=$1
    }

    config-mem()
    {
	_MEM=$1
    }

    config-nr-vms()
    {
	_NR_VMS=$1
    }

    config-timeout()
    {
	n=$1
	if [ "${EXTENDED_DEBUG:-0}" == 1 ]; then
	    n=$((n * 2))
	fi
	_TIMEOUT=$n
    }

    . "$TESTPROG"

    if [ -z "$_MEM" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$_TIMEOUT" ]; then
	echo "test must specify config-timeout"
	exit 1
    fi
}
