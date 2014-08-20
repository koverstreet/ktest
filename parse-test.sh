
parse_test_deps()
{
    _CPUS="6"
    _MEM=""
    _TIMEOUT=""
    _SCRATCH=""
    _KERNEL_CONFIG_REQUIRE=""
    _CONTAINERS=""
    _INFINIBAND=""
    _VMCLUSTER=""

    local TEST=$1
    local TESTDIR=$(dirname "$TEST")
    local HAVE_CONTAINER=""

    ktest_priority=$PRIORITY

    _add-file()
    {
	if [ ! -e "$1" ]; then
	    echo "Dependency $req not found"
	    exit 1
	fi

	FILES+=" $(readlink -f "$1")"
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

	. "$f" deps
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
	local req=$2

	if [ "${makefile:0:1}" = "/" ]; then
	    local f="$makefile"
	else
	    local f="$TESTDIR/$makefile"
	fi

	local dir="$(dirname "$f")"

	(cd "$dir"; make -f "$f" "$req")

	_add-file "$dir/$req"
    }

    require-container()
    {
	_CONTAINERS+=" $1"
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

    config-vmcount()
    {
	# what's going on here?
	_VMCLUSTER="--cluster $1"
    }

    config-timeout()
    {
	_TIMEOUT=$1
    }

    PATH+=":/sbin:/usr/sbin:/usr/local/sbin"

    . "$TEST" deps

    if [ -z "$_MEM" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$_TIMEOUT" ]; then
	echo "test must specify config-timeout"
	exit 1
    fi
}
