
parse_test_deps()
{
    _CPUS="6"
    _MEM=""
    _TIMEOUT=""
    _SCRATCH=""
    _KERNEL_CONFIG_REQUIRE=""
    _INFINIBAND=""
    _KERNEL_APPEND=""
    _NR_VMS="1"
    TEST_RUNNING=""

    local TEST=$1
    local TESTDIR="$(dirname "$TEST")"

    ktest_priority=$PRIORITY

    _add-file()
    {
	if [ ! -e "$1" ]; then
	    echo "Dependency $req not found"
	    exit 1
	fi

	FILES+=("$(readlink -f "$1")")
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

    require-bin()
    {
	local req=$1
	local f="$(which "$req")"

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

    require-kernel-config()
    {
	_KERNEL_CONFIG_REQUIRE+=",$1"
    }

    config-scratch-devs()
    {
	if [ "$_SCRATCH" == "" ]; then
	    _SCRATCH="-s $1"
	else
	    _SCRATCH+=",$1"
	fi
    }

    config-cpus()
    {
	_CPUS=$1
    }

    config-mem()
    {
	_MEM=$1
    }

    config-infiniband()
    {
	_INFINIBAND="--conx3"

	require-kernel-config MLX4_EN
	require-kernel-config MLX4_INFINIBAND
	require-kernel-config INFINIBAND_MTHCA
	require-kernel-config INFINIBAND_USER_MAD
	require-kernel-config INFINIBAND_USER_ACCESS
	require-kernel-config INFINIBAND_IPOIB
    }

    config-nr-vms()
    {
	_NR_VMS=$1
    }

    config-timeout()
    {
	_TIMEOUT=$1
    }

    require-kernel-append()
    {
	_KERNEL_APPEND+=" $1"
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
