
set -o nounset
set -o errtrace
set -o pipefail

trap 'echo "Error $? at $BASH_SOURCE $LINENO from: $BASH_COMMAND, exiting"' ERR

ktest_tmp=${ktest_tmp:-""}
ktest_exit()
{
    local children=$(jobs -rp)
    if [[ -n $children ]]; then
	kill -9 $children >& /dev/null
	wait $(jobs -rp) >& /dev/null || true
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
    if $ktest_verbose; then
	echo "$@"
    fi
}

run_quiet()
{
    local msg=$1
    shift

    if $ktest_verbose; then
	if [[ -n $msg ]]; then
	    echo "$msg:"
	fi
	"$@"
    else
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
	aarch64|arm64)
	    ktest_arch=aarch64
	    DEBIAN_ARCH=arm64
	    ARCH_TRIPLE=aarch64-linux-gnu

	    KERNEL_ARCH=arm64
	    BITS=64

	    QEMU_PACKAGE=qemu-system-arm
	    QEMU_BIN=qemu-system-aarch64
	    ;;
	mips)
	    DEBIAN_ARCH=mips
	    ARCH_TRIPLE=mips-linux-gnu

	    KERNEL_ARCH=mips
	    BITS=32

	    QEMU_PACKAGE=qemu-system-mips
	    QEMU_BIN=qemu-system-mips
	    ;;
	mips64)
	    DEBIAN_ARCH=mips
	    ARCH_TRIPLE=mips-linux-gnu

	    KERNEL_ARCH=mips
	    BITS=64

	    QEMU_PACKAGE=qemu-system-mips
	    QEMU_BIN=qemu-system-mips64
	    ;;
	sparc)
	    DEBIAN_ARCH=sparc
	    ARCH_TRIPLE=sparc64-linux-gnu

	    KERNEL_ARCH=sparc
	    BITS=32

	    QEMU_PACKAGE=qemu-system-sparc
	    QEMU_BIN=qemu-system-sparc
	    ;;
	sparc64)
	    DEBIAN_ARCH=sparc
	    ARCH_TRIPLE=sparc64-linux-gnu

	    KERNEL_ARCH=sparc
	    BITS=64

	    QEMU_PACKAGE=qemu-system-sparc
	    QEMU_BIN=qemu-system-sparc64
	    ;;
	ppc|powerpc)
	    DEBIAN_ARCH=powerpc
	    MIRROR=http://deb.debian.org/debian-ports

	    ARCH_TRIPLE=powerpc-linux-gnu

	    KERNEL_ARCH=powerpc
	    BITS=32

	    QEMU_PACKAGE=qemu-system-ppc
	    QEMU_BIN=qemu-system-ppc
	    ;;
	ppc64)
	    DEBIAN_ARCH=ppc64
	    MIRROR=http://deb.debian.org/debian-ports

	    ARCH_TRIPLE=powerpc-linux-gnu

	    KERNEL_ARCH=powerpc
	    BITS=64

	    QEMU_PACKAGE=qemu-system-ppc
	    QEMU_BIN=qemu-system-ppc64
	    ;;
	*)
	    echo "Unsupported architecture $1"
	    exit 1
    esac

    if [[ $ktest_arch != $(uname -m) ]]; then
	CROSS_COMPILE=1
    fi
}

find_command() {
    command -v $1 >/dev/null 2>&1
}

checkdep() {
    local dep=$1
    local package=$dep
    [[ $# -ge 2 ]] && package=$2

    if find_command "$dep"; then
	return
    else
	echo "$dep" not found!
    fi

    if find_command sudo && find_command apt-get ; then
	echo "  installing $package:"
	sudo apt-get -qq install --no-install-recommends "$package"
    elif find_command sudo && find_command pacman ; then
	echo "  installing $package:"
	sudo pacman -S --noconfirm "$package"
    else
	echo "  please install"
	exit 1
    fi
}
