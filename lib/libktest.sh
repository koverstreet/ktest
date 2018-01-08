
. "$ktest_dir/lib/util.sh"
. "$ktest_dir/lib/parse-test.sh"

VMSTART=("$ktest_dir/vm-start")

ktest_priority=0		# hint for how long test should run
ktest_image=""			# root image that will be booted
                        #       set with: -i <path>
                        #       defaults: /var/lib/ktest/root
                        #       auto-override: $HOME/.ktest/root
ktest_kernel=""		# dir that has the kernel to run
                        #       set with: -k <path>
ktest_idfile=""		# passed as --id to vmstart
                        #       set with: -w <path>
ktest_out=""		# dir for test output (logs, code coverage, etc.)
ktest_vmdir="/tmp"	# dir where scratch drives are created
                        #       defaults: /tmp
                        #       auto-override: $HOME/.ktest/tmp
ktest_interactive=0     # if set to 1, timeout is ignored completely
                        #       sets with: -I
ktest_exit_on_success=0	# if true, exit on success, not failure or timeout
ktest_failfast=0
ktest_loop=0
ktest_verbose=0		# if false, append quiet to kernel commad line

checkdep genisoimage
checkdep minicom
checkdep socat
checkdep qemu-system-x86_64 qemu-system-i386

# config files:
[[ -f $ktest_dir/ktestrc ]]	&& . "$ktest_dir/ktestrc"
[[ -f /etc/ktestrc ]]		&& . /etc/ktestrc
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"

# defaults:
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"
[[ -f $HOME/.ktest/root ]]	&& ktest_image="$HOME/.ktest/root"
[[ -d $HOME/.ktest/tmp ]]	&& ktest_vmdir="$HOME/.ktest/tmp"

ktest_args="a:p:i:k:ISw:s:o:flvx"
parse_ktest_arg()
{
    local arg=$1

    case $arg in
	a)
	    ARCH=$OPTARG
	    ;;
	p)
	    ktest_priority=$OPTARG
	    ;;
	i)
	    ktest_image=$OPTARG
	    ;;
	k)
	    ktest_kernel=$OPTARG
	    ;;
	w)
	    ktest_idfile="$OPTARG"
	    ;;
	s)
	    VMSTART+=(--scratchdir="$OPTARG")
	    ;;
	o)
	    ktest_out="$OPTARG"
	    ;;
	I)
	    ktest_interactive=1
	    ;;
	S)
	    ktest_exit_on_success=1
	    ;;
	f)
	    ktest_failfast=1
	    ;;
	l)
	    ktest_loop=1
	    ;;
	v)
	    ktest_verbose=1
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

    [[ -z $ktest_image ]]	&& ktest_image=/var/lib/ktest/root.$DEBIAN_ARCH
    [[ -z $ktest_idfile ]]	&& ktest_idfile=./.ktest-vm
    [[ -z $ktest_out ]]		&& OUTPUT_DIR=./ktest-out

    VMSTART+=(--idfile="$ktest_idfile")
    VMSTART+=(--tmpdir="$ktest_vmdir")

    ktest_kernel=$(readlink -f "$ktest_kernel")
    ktest_out=$(readlink -f "$OUTPUT_DIR")
}

ktest_run()
{
    VMSTART+=("start")

    if [[ -z $ktest_kernel ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    if [[ ! -f $ktest_image ]]; then
	echo "VM root filesystem not found, use vm_create_image to create one"
	exit 1
    fi

    if [[ $# = 0 ]]; then
	echo "ktest: missing test"
	exit 1
    fi

    local ktest_test=$(readlink -e "$1")
    shift
    local ktest_testargs="$@"
    local home=$HOME

    get_tmpdir

    BUILD_DEPS=1
    parse_test_deps "$ktest_test"

    mkdir -p "$ktest_out"

    if [[ $ktest_exit_on_success = 1 || $ktest_interactive = 1 ]]; then
	case $KERNEL_ARCH in
	    x86)
		VMSTART+=("--kgdb")
		;;
	esac
    fi

    local ktest_tmp=$TMPDIR
    local ktest_crashdump=0
    [[ $ktest_interactive = 0 ]]	&& ktest_crashdump=1

    set|grep -vE '^[A-Z]' > "$TMPDIR/env"

    [[ $ktest_verbose = 0 ]]	&& VMSTART+=(--append="quiet systemd.show_status=0")

    [[ $ktest_crashdump = 1 ]]	&& VMSTART+=(--append="crashkernel=128M")

    VMSTART+=(--architecture="${QEMU_BIN#qemu-system-}")
    VMSTART+=(--image="$ktest_image")
    VMSTART+=(--kernel="$ktest_kernel/vmlinuz")
    VMSTART+=(--fs "/" host)
    VMSTART+=(--append=ktest.dir="$ktest_dir")
    VMSTART+=(--append=ktest.env="$TMPDIR/env")
    VMSTART+=(--append=log_buf_len=8M)
    VMSTART+=(--memory="$ktest_mem")
    VMSTART+=(--cpus="$ktest_cpus")
    VMSTART+=(--nr_vms="$_NR_VMS")
    VMSTART+=("${_VMSTART_ARGS[@]}")

    set +o errexit

    if [[ $ktest_interactive = 1 ]]; then
	"${VMSTART[@]}"
    elif [[ $ktest_exit_on_success = 1 ]]; then
	"${VMSTART[@]}"|sed -u -e '/TEST SUCCESS/ { p; Q7 }'
    else
	timeout --foreground "$((60 + ktest_timeout))" "${VMSTART[@]}"|
	    $ktest_dir/catch_test_success.awk
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
    ktest_interactive=1

    ktest_run "$ktest_dir/boot.ktest" "$@"
}

ktest_ssh()
{
    vmdir=$(<$ktest_idfile)
    sock=$vmdir/net-0
    ip="10.0.2.2"

    (cd "$ktest_dir/lib"; make lwip-connect) > /dev/null

    exec ssh -t -F /dev/null						\
	-o CheckHostIP=no						\
	-o StrictHostKeyChecking=no					\
	-o UserKnownHostsFile=/dev/null					\
	-o NoHostAuthenticationForLocalhost=yes				\
	-o ServerAliveInterval=2					\
	-o ControlMaster=auto						\
	-o ControlPath="$vmdir/controlmaster"				\
	-o ControlPersist=yes						\
	-o ProxyCommand="$ktest_dir/lib/lwip-connect $sock $ip 22"	\
	root@127.0.0.1 "$@"
}

ktest_gdb()
{
    if [[ -z $ktest_kernel ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    vmdir=$(<$ktest_idfile)

    exec gdb -ex "set remote interrupt-on-connect"			\
	     -ex "target remote | socat UNIX-CONNECT:$vmdir/vm-0-gdb -"	\
	     "$ktest_kernel/vmlinux"
}

ktest_kgdb()
{
    if [[ -z $ktest_kernel ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    ktest_sysrq g

    vmdir=$(<$ktest_idfile)

    exec gdb -ex "set remote interrupt-on-connect"			\
	     -ex "target remote | socat UNIX-CONNECT:$vmdir/vm-0-kgdb -"\
	     "$ktest_kernel/vmlinux"
}

ktest_mon()
{
    vmdir=$(<$ktest_idfile)

    exec minicom -D "unix#$vmdir/vm-0-mon"
}

ktest_sysrq()
{
    key=$1
    vmdir=$(<$ktest_idfile)

    echo sendkey alt-sysrq-$key | socat - "UNIX-CONNECT:$vmdir/vm-0-mon"
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
