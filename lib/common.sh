
set -o nounset
set -o errtrace
set -o pipefail

[[ -v ktest_dir ]] || ktest_dir=$(dirname ${BASH_SOURCE})/..

. "$ktest_dir/cross.conf"

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

parse_arch()
{
    case $1 in
	x86|i386|i686)
	    ktest_arch=x86
	    DEBIAN_ARCH=i386
	    ARCH_TRIPLE=${ARCH_TRIPLE_X86}
	    RUST_TRIPLE=i686-unknown-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=32

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-i386
	    ;;
	x86_64|amd64)
	    ktest_arch=x86_64
	    DEBIAN_ARCH=amd64
	    ARCH_TRIPLE=${ARCH_TRIPLE_X86_64}
	    RUST_TRIPLE=x86_64-unknown-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=64

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-x86_64
	    ;;
	aarch64|arm64)
	    ktest_arch=aarch64
	    DEBIAN_ARCH=arm64
	    ARCH_TRIPLE=${ARCH_TRIPLE_ARM64}
	    RUST_TRIPLE=aarch64-unknown-linux-gnu

	    KERNEL_ARCH=arm64
	    BITS=64

	    QEMU_PACKAGE=qemu-system-arm
	    QEMU_BIN=qemu-system-aarch64
	    ;;
	armhf|armv7|armv7l|arm)
	    ktest_arch=arm
	    DEBIAN_ARCH=armhf
	    ARCH_TRIPLE=${ARCH_TRIPLE_ARMV7}
	    RUST_TRIPLE=armv7-unknown-linux-gnueabihf

	    KERNEL_ARCH=arm
	    BITS=32

	    QEMU_PACKAGE=qemu-system-arm
	    QEMU_BIN=qemu-system-arm
	    ;;
	s390x)
	    DEBIAN_ARCH=s390x
	    ARCH_TRIPLE=${ARCH_TRIPLE_S390X}
	    RUST_TRIPLE=s390x-unknown-linux-gnu

	    KERNEL_ARCH=s390
	    BITS=64

	    QEMU_PACKAGE=qemu-system-s390x
	    QEMU_BIN=qemu-system-s390x
	    ;;
	riscv64)
	    DEBIAN_ARCH=riscv64
	    ARCH_TRIPLE=${ARCH_TRIPLE_RISCV64}
	    MIRROR=http://deb.debian.org/debian-ports
	    RUST_TRIPLE=riscv64gc-unknown-linux-gnu

	    KERNEL_ARCH=riscv
	    BITS=64

	    QEMU_PACKAGE=qemu-system-riscv
	    QEMU_BIN=qemu-system-riscv64
	    ;;
	sparc64)
	    DEBIAN_ARCH=sparc64
	    ARCH_TRIPLE=${ARCH_TRIPLE_SPARC64}
	    MIRROR=http://deb.debian.org/debian-ports
	    RUST_TRIPLE=sparc64-unknown-linux-gnu

	    KERNEL_ARCH=sparc
	    BITS=64

	    QEMU_PACKAGE=qemu-system-sparc
	    QEMU_BIN=qemu-system-sparc64
	    ;;
	ppc64|powerpc)
	    ktest_arch=ppc64
	    DEBIAN_ARCH=ppc64
	    MIRROR=http://deb.debian.org/debian-ports

	    ARCH_TRIPLE=${ARCH_TRIPLE_PPC64}
	    RUST_TRIPLE=powerpc64-unknown-linux-gnu

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
    #special case: x86_64 is able to run i386 code.  this isn't always the case for armv8 -> armv7 (cortex A35)
    [[ $DEBIAN_ARCH == "i386" && "$(uname -m)" == "x86_64" ]] && unset CROSS_COMPILE
    export DEBIAN_ARCH
    export MIRROR
    export ARCH_TRIPLE
    export KERNEL_ARCH
    export QEMU_PACKAGE
    export QEMU_BIN
    export ktest_arch
    export BITS
    export RUST_TRIPLE
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
