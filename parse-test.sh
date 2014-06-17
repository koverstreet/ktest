
parse_test_deps()
{
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
	local req=$1
	local f=$2

	if [ ! -e "$f" ]; then
	    echo "Dependency $req not found"
	    exit 1
	fi

	FILES+=" $(readlink -f "$f")"
    }

    require-lib()
    {
	local req="$1"
	local f="$TESTDIR/$req"

	if [ "${req:0:1}" = "/" ]; then
	    local f="$req"
	else
	    local f="$TESTDIR/$req"
	fi

	_add-file "$req" "$f"

	. "$f" deps
    }

    require-bin()
    {
	local req=$1
	local f="$(which "$req")"

	_add-file "$req" "$f"
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
	_SCRATCH="-s $1"
    }

    config-mem()
    {
	_MEM=$1
    }

    config-infiniband()
    {
	_INFINIBAND="--conx3"
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
