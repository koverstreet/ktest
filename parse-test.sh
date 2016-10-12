
parse_test_deps()
{
    _CPUS="6"
    _MEM=""
    _TIMEOUT=""
    _KERNEL_CONFIG_REQUIRE=""
    _KERNEL_APPEND=""
    _NR_VMS="1"
    _VMSTART_ARGS=(" ")
    TEST_RUNNING=""

    local NEXT_SCRATCH_DEV="b"
    local TEST=$1
    local TESTDIR="$(dirname "$TEST")"

    ktest_priority=$PRIORITY

    _add-file()
    {
	if [ ! -e "$1" ]; then
	    echo "Dependency $1 not found"
	    exit 1
	fi

	# Make sure directories show up, not just their contents
	FILES+=("$(basename "$1")=$(readlink -f "$1")")
    }

    require-lib()
    {
	local req="$1"

	if [ "${req:0:1}" = "/" ]; then
	    local f="$req"
	else
	    local f="$TESTDIR/$req"
	fi

	_add-file "$f"

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
	local path=$(readlink -e "$TESTDIR/$req")

	checkdep debuild devscripts

	if ! [[ -d $path ]]; then
	    echo "build-deb dependency $req not found"
	    exit 1
	fi

	get_tmpdir
	local out="$TMPDIR/out"

	pushd "$path"	> /dev/null
	if ! make > "$out" 2>$1 && [[ $? -eq 2 ]]; then
	    echo "Error building $req:"
	    cat "$out"
	    exit 1
	fi

	if ! debuild -nc -us -uc -i -I	> "$out" 2>$1; then
	    echo "Error creating package for $req: $?"
	    cat "$out"
	    exit 1
	fi
	popd		> /dev/null

	for deb in $path*.deb; do
	    _add-file "$deb"
	done
    }

    require-bin()
    {
	local req=$1
	local f="$(which "$req")"

	if [[ -z $f ]]; then
	    echo "Dependency $req not found"
	    exit 1
	fi

	_add-file "$f"
    }

    require-make()
    {
	local makefile=$1
	shift
	local req=( "$@" )

	if [ "${makefile:0:1}" = "/" ]; then
	    local f="$makefile"
	else
	    local f="$TESTDIR/$makefile"
	fi

	local dir="$(dirname "$f")"

	for i in ${req[*]} ; do
	    (cd "$dir"; make -f "$(basename "$f")" "$i")
	    _add-file "$dir/$i"
	done
    }

    require-file()
    {
	local file=$1

	if [ "${file:0:1}" = "/" ]; then
	    local f="$file"
	else
	    local f="$TESTDIR/$file"
	fi

	_add-file "$f"
    }

    require-kernel-config()
    {
	_KERNEL_CONFIG_REQUIRE+=",$1"
    }

    require-kernel-append()
    {
	_KERNEL_APPEND+=" $1"
    }

    scratch-dev()
    {
	local dev_size=$1
	local dev_path="/dev/sd$NEXT_SCRATCH_DEV"

	NEXT_SCRATCH_DEV=$(echo $NEXT_SCRATCH_DEV|tr "a-z" "b-z_")

	_VMSTART_ARGS+=("--scratchdev" "$dev_size")

	echo "$dev_path"
    }

    config-scratch-devs()
    {
	_VMSTART_ARGS+=("--scratchdev" "$1")
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


    PATH+=":/sbin:/usr/sbin:/usr/local/sbin"

    . "$TEST"

    if [ -z "$_MEM" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$_TIMEOUT" ]; then
	echo "test must specify config-timeout"
	exit 1
    fi
}
