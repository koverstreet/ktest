
set -o nounset
set -o errtrace
set -o errtrace

trap 'echo "Error $? from: $BASH_COMMAND, exiting" >&2' ERR

ktest_tmp=${ktest_tmp:-""}
ktest_exit()
{
    local children=$(jobs -rp)
    if [[ -n $children ]]; then
	kill -9 $children >& /dev/null
	wait $(jobs -rp) >& /dev/null
    fi

    [[ -n $ktest_tmp ]] && rm -rf "$ktest_tmp"
    true
}

trap ktest_exit EXIT

get_tmpdir()
{
    if [[ -z $ktest_tmp ]]; then
	ktest_tmp=$(mktemp --tmpdir -d $(basename "$0")-XXXXXXXXXX)
    fi
}

log_verbose()
{
    if [[ $ktest_verbose != 0 ]]; then
	echo "$@"
    fi
}

run_quiet()
{
    local msg=$1
    shift

    if [[ $ktest_verbose = 0 ]]; then
	if [[ -n $msg ]]; then
	    echo -n "$msg... "
	fi

	get_tmpdir
	local out="$ktest_tmp/out-$msg"

	set +e
	(set -e; "$@") > "$out" 2>&1
	local ret=$?
	set -e

	if [[ $ret != 0 ]]; then
	    echo
	    cat "$out"
	    exit 1
	fi

	if [[ -n $msg ]]; then
	    echo done
	fi
    else
	if [[ -n $msg ]]; then
	    echo "$msg:"
	fi
	"$@"
    fi
}

join_by()
{
    local IFS="$1"
    shift
    echo "$*"
}

ktest_arch=$(uname -m)
CROSS_COMPILE=""

parse_arch()
{
    case $1 in
	x86|i386)
	    ktest_arch=x86
	    DEBIAN_ARCH=i386
	    ARCH_TRIPLE=x86-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=32

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-x86_64
	    ;;
	x86_64|amd64)
	    ktest_arch=x86_64
	    DEBIAN_ARCH=amd64
	    ARCH_TRIPLE=x86_64-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=64

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-x86_64
	    ;;
	mips)
	    DEBIAN_ARCH=mips
	    ARCH_TRIPLE=mips-linux-gnu

	    KERNEL_ARCH=mips
	    BITS=32

	    QEMU_PACKAGE=qemu-system-mips
	    QEMU_BIN=qemu-system-mips
	    CROSS_COMPILE=1
	    ;;
	mips64)
	    DEBIAN_ARCH=mips
	    ARCH_TRIPLE=mips-linux-gnu

	    KERNEL_ARCH=mips
	    BITS=64

	    QEMU_PACKAGE=qemu-system-mips
	    QEMU_BIN=qemu-system-mips64
	    CROSS_COMPILE=1
	    ;;
	sparc)
	    DEBIAN_ARCH=sparc
	    ARCH_TRIPLE=sparc64-linux-gnu

	    KERNEL_ARCH=sparc
	    BITS=32

	    QEMU_PACKAGE=qemu-system-sparc
	    QEMU_BIN=qemu-system-sparc
	    CROSS_COMPILE=1
	    ;;
	sparc64)
	    DEBIAN_ARCH=sparc
	    ARCH_TRIPLE=sparc64-linux-gnu

	    KERNEL_ARCH=sparc
	    BITS=64

	    QEMU_PACKAGE=qemu-system-sparc
	    QEMU_BIN=qemu-system-sparc64
	    CROSS_COMPILE=1
	    ;;
	ppc|powerpc)
	    DEBIAN_ARCH=powerpc
	    MIRROR=http://deb.debian.org/debian-ports

	    ARCH_TRIPLE=powerpc-linux-gnu

	    KERNEL_ARCH=powerpc
	    BITS=32

	    QEMU_PACKAGE=qemu-system-ppc
	    QEMU_BIN=qemu-system-ppc
	    CROSS_COMPILE=1
	    ;;
	ppc64)
	    DEBIAN_ARCH=ppc64
	    MIRROR=http://deb.debian.org/debian-ports

	    ARCH_TRIPLE=powerpc-linux-gnu

	    KERNEL_ARCH=powerpc
	    BITS=64

	    QEMU_PACKAGE=qemu-system-ppc
	    QEMU_BIN=qemu-system-ppc64
	    CROSS_COMPILE=1
	    ;;
	*)
	    echo "Unsupported architecture $1"
	    exit 1
    esac

#    if [[ $ktest_arch != $(uname -m) ]]; then
#	CROSS_COMPILE=1
#    fi
}

checkdep()
{
    local dep=$1
    local package=$dep

    if [[ $# -ge 2 ]]; then
	package=$2
    else
	package=$dep
    fi

    local found=0

    if [[ ${dep:0:1} = / ]]; then
	# absolute path
	[[ -e $dep ]] && found=1
    else
	which "$dep" > /dev/null 2>&1 && found=1
    fi

    if [[ $found = 0 ]]; then
	echo -n "$dep not found"

	if which apt-get > /dev/null 2>&1 && \
	    which sudo > /dev/null 2>&1; then
		    echo ", installing $package:"
		    sudo apt-get -qq install --no-install-recommends "$package"
		else
		    echo ", please install"
		    exit 1
	fi
    fi
}
