
checkdep()
{
	COMMAND=$1

	if [[ $# -ge 2 ]]; then
	    PACKAGE=$2
	else
	    PACKAGE=$COMMAND
	fi

	if ! which "$COMMAND" > /dev/null; then
		echo -n "$COMMAND not found"

		if which apt-get > /dev/null && \
			which sudo > /dev/null; then
			echo ", installing $PACKAGE:"
			sudo apt-get install -y "$PACKAGE"
		else
			echo ", please install"
			exit 1
		fi
	fi
}

# scratch dir cleaned up on exit
TMPDIR=""

cleanup_tmpdir()
{
    [[ -n $TMPDIR ]] && rm -rf "$TMPDIR"
}

get_tmpdir()
{
    if [[ -z $TMPDIR ]]; then
	TMPDIR=$(mktemp --tmpdir -d $(basename "$0")-XXXXXXXXXX)
	trap 'rm -rf "$TMPDIR"' EXIT
    fi
}

ARCH=x86_64

parse_arch()
{
    case $1 in
	x86|i386)
	    DEBIAN_ARCH=i386
	    ARCH_TRIPLE=x86-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=32

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-x86_64
	    ;;
	x86_64|amd64)
	    DEBIAN_ARCH=amd64
	    ARCH_TRIPLE=x86_64-linux-gnu

	    KERNEL_ARCH=x86
	    BITS=64

	    QEMU_PACKAGE=qemu-system-x86
	    QEMU_BIN=qemu-system-x86_64
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
	    ARCH_TRIPLE=powerpc-linux-gnu

	    KERNEL_ARCH=powerpc
	    BITS=32

	    QEMU_PACKAGE=qemu-system-ppc
	    QEMU_BIN=qemu-system-ppc
	    ;;
	ppc64)
	    DEBIAN_ARCH=ppc64
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
}

#debian_arch()
#{
#    local -A map=([x86]=i386 [x86_64]=amd64)
#
#    if [[ ${map[$1]+_} ]]; then
#	echo ${map[$1]}
#    else
#	echo $1
#    fi
#}
#
#kernel_arch()
#{
#    local -A map=([x86_64]=x86)
#
#    if [[ ${map[$1]+_} ]]; then
#	echo ${map[$1]}
#    else
#	echo $1
#    fi
#}
#
#qemu_arch()
#{
#    local -A map=([x86_64]=x86 [powerpc]=ppc)
#
#    if [[ ${map[$1]+_} ]]; then
#	echo ${map[$1]}
#    else
#	echo $1
#    fi
#}
#
#arch_to_triple()
#{
#    local -A map=([x86]=x86_64 [sparc]=sparc64)
#
#    if [[ ${map[$1]+_} ]]; then
#	echo ${map[$1]}-linux-gnu
#    else
#	echo $1-linux-gnu
#    fi
#}

list_descendants()
{
  local children=$(ps -o pid= --ppid "$1")

  for pid in $children; do
    list_descendants "$pid"
  done

  echo "$children"
}

join_by()
{
    local IFS="$1"
    shift
    echo "$*"
}
