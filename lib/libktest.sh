
. "$KTESTDIR/lib/parse-test.sh"

VMSTART=("$KTESTDIR/vm-start")

PRIORITY=0		# hint for how long test should run
IMG=""			# root image that will be booted
                        #       set with: -i <path>
                        #       defaults: /var/lib/ktest/root
                        #       auto-override: $HOME/.ktest/root
KERNEL=""		# dir that has the kernel to run
                        #       set with: -k <path>
IDFILE=""		# passed as --id to vmstart
                        #       set with: -w <path>
OUTPUT_DIR=""		# dir for test output (logs, code coverage, etc.)
VM_TMPDIR="/tmp"	# dir where scratch drives are created
                        #       defaults: /tmp
                        #       auto-override: $HOME/.ktest/tmp
INTERACTIVE=0           # if set to 1, timeout is ignored completely
                        #       sets with: -I
EXIT_ON_SUCCESS=0	# if true, exit on success, not failure or timeout
FAILFAST=0
LOOP=0
VERBOSE=0		# if false, append quiet to kernel commad line

checkdep genisoimage
checkdep minicom
checkdep socat
checkdep qemu-system-x86_64 qemu-system-i386

# config files:
[[ -f $KTESTDIR/ktestrc ]]	&& . "$KTESTDIR/ktestrc"
[[ -f /etc/ktestrc ]]		&& . /etc/ktestrc

[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"
[[ -f $HOME/.ktest/root ]]	&& IMG="$HOME/.ktest/root"
[[ -d $HOME/.ktest/tmp ]]	&& VM_TMPDIR="$HOME/.ktest/tmp"

ktest_args="a:p:i:k:ISw:s:o:flvx"
parse_ktest_arg()
{
    local arg=$1

    case $arg in
	a)
	    ARCH=$OPTARG
	    ;;
	p)
	    PRIORITY=$OPTARG
	    ;;
	i)
	    IMG=$OPTARG
	    ;;
	k)
	    KERNEL=$OPTARG
	    ;;
	w)
	    IDFILE="$OPTARG"
	    ;;
	s)
	    VMSTART+=(--scratchdir="$OPTARG")
	    ;;
	o)
	    OUTPUT_DIR="$OPTARG"
	    ;;
	I)
	    INTERACTIVE=1
	    ;;
	S)
	    EXIT_ON_SUCCESS=1
	    ;;
	f)
	    FAILFAST=1
	    ;;
	l)
	    LOOP=1
	    ;;
	v)
	    VERBOSE=1
	    ;;
	x)
	    set -x
	    ;;
    esac
}

parse_args_post()
{
    parse_arch "$ARCH"

    checkdep $QEMU_BIN $QEMU_PACKAGE

    [[ -z $IMG ]]		&& IMG=/var/lib/ktest/root.$DEBIAN_ARCH
    [[ -z $IDFILE ]]		&& IDFILE=./.ktest-vm
    [[ -z $OUTPUT_DIR ]]	&& OUTPUT_DIR=./ktest-out

    VMSTART+=(--idfile="$IDFILE")
    VMSTART+=(--tmpdir="$VM_TMPDIR")

    KERNEL=$(readlink -f "$KERNEL")
    OUTPUT_DIR=$(readlink -f "$OUTPUT_DIR")
}

ktest_run()
{
    local CRASHDUMP=0
    [[ $INTERACTIVE = 0 ]]  && CRASHDUMP=1

    VMSTART+=("start")

    if [[ -z $KERNEL ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    if [[ ! -f $IMG ]]; then
	echo "VM root filesystem not found, use vm_create_image to create one"
	exit 1
    fi

    if [[ $# = 0 ]]; then
	echo "ktest: missing test"
	exit 1
    fi

    local TEST=$(readlink -e "$1")
    shift

    get_tmpdir
    local testargs="$TMPDIR/testargs"
    echo "$@" > $testargs

    BUILD_DEPS=1
    parse_test_deps "$TEST"

    mkdir -p "$OUTPUT_DIR"

    if [[ $EXIT_ON_SUCCESS = 1 || $INTERACTIVE = 1 ]]; then
	case $KERNEL_ARCH in
	    x86)
		VMSTART+=("--kgdb")
		;;
	esac
    fi

    [[ $EXIT_ON_SUCCESS = 0 && $INTERACTIVE = 0 ]] &&			\
	VMSTART+=(--append="ktest.timeout=$_TIMEOUT")

    [[ $VERBOSE = 0 ]]	    && VMSTART+=(--append="quiet systemd.show_status=0")

    [[ $CRASHDUMP = 1 ]]    && VMSTART+=(--append="crashkernel=128M")
    VMSTART+=(--append=ktest.crashdump=$CRASHDUMP)

    VMSTART+=(--architecture="${QEMU_BIN#qemu-system-}")
    VMSTART+=(--image="$IMG")
    VMSTART+=(--kernel="$KERNEL/vmlinuz")

    VMSTART+=(--fs "/" host)
    VMSTART+=(--append=ktest.dir="$KTESTDIR")
    VMSTART+=(--append=ktest.kernel="$KERNEL")
    VMSTART+=(--append=ktest.test="$TEST")
    VMSTART+=(--append=ktest.out="$OUTPUT_DIR")
    VMSTART+=(--append=ktest.tmp="$TMPDIR")

    VMSTART+=(--append=ktest.priority="$PRIORITY")
    VMSTART+=(--append=ktest.failfast="$FAILFAST")
    VMSTART+=(--append=ktest.loop="$LOOP")
    VMSTART+=(--append=ktest.verbose="$VERBOSE")
    VMSTART+=(--append=ktest.testargs="$testargs")

    VMSTART+=(--append=log_buf_len=8M)
    VMSTART+=(--memory="$_MEM")
    VMSTART+=(--cpus="$_CPUS")
    VMSTART+=(--nr_vms="$_NR_VMS")
    VMSTART+=("${_VMSTART_ARGS[@]}")

    set +o errexit

    if [[ $INTERACTIVE = 1 ]]; then
	"${VMSTART[@]}"
    elif [[ $EXIT_ON_SUCCESS = 1 ]]; then
	"${VMSTART[@]}"|sed -u -e '/TEST SUCCESS/ { p; Q7 }'
    else
	timeout --foreground "$((60 + _TIMEOUT))" "${VMSTART[@]}"|
	    $KTESTDIR/catch_test_success.awk
    fi

    ret=$?

    if [[ $ret = 124 ]]; then
	echo 'TEST TIMEOUT'
	exit 1
    fi

    # don't want sed exiting normally (saw neither TEST SUCCESS nor TEST FAILED)
    # to be consider success:
    [[ $ret = 7 ]]
}

ktest_boot()
{
    INTERACTIVE=1

    ktest_run "$KTESTDIR/boot.ktest" "$@"
}

ktest_ssh()
{
    exec "${VMSTART[@]}" ssh "$@"
}

ktest_gdb()
{
    if [[ -z $KERNEL ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    exec "${VMSTART[@]}" gdb "$KERNEL/vmlinux"
}

ktest_kgdb()
{
    if [[ -z $KERNEL ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    "${VMSTART[@]}" sysrq g

    exec "${VMSTART[@]}" kgdb "$KERNEL/vmlinux"
}

ktest_mon()
{
    exec "${VMSTART[@]}" mon
}

ktest_sysrq()
{
    exec "${VMSTART[@]}" sysrq "$@"
}

ktest_usage_cmds()
{
    echo "  boot        Boot a VM without running anything"
    echo "  run <test>  Run a kernel test"
    echo "  ssh         Login as root"
    echo "  gdb         Connect to qemu's gdb interface"
    echo "  kgdb        Connect to kgdb"
    echo "  mon         Connect to qemu monitor"
    echo "  sysrq <key> Send magic sysrq key via monitor"
}

ktest_usage_post()
{
    echo "For kgdb to be enabled, either -I or -S must be specified"
}
