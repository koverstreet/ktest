
. "$ktest_dir/lib/util.sh"
. "$ktest_dir/lib/parse-test.sh"

if [[ $(id -u) = 0 ]] ; then
    echo $0 should not be run as root
    exit 1
fi

ktest_root_image=""	# virtual machine root filesystem
                        #       set with: -i <path>
                        #       defaults: /var/lib/ktest/root
                        #       auto-override: $HOME/.ktest/root
ktest_kernel_binary=""		# dir that has the kernel to run
                        #       set with: -k <path>
ktest_out=""		# dir for test output (logs, code coverage, etc.)

ktest_vmdir=""		# symlink to actual vm dir
ktest_priority=0	# hint for how long test should run
ktest_interactive=0     # if set to 1, timeout is ignored completely
                        #       sets with: -I
ktest_exit_on_success=0	# if true, exit on success, not failure or timeout
ktest_failfast=0
ktest_loop=0
ktest_verbose=0		# if false, append quiet to kernel commad line
ktest_crashdump=0
ktest_kgdb=0

ktest_storage_bus=virtio-scsi-pci

checkdep minicom
checkdep socat
checkdep qemu-system-x86_64	qemu-system-x86
checkdep vde_switch		vde2
checkdep /usr/include/lwipv6.h	liblwipv6-dev

# config files:
[[ -f $ktest_dir/ktestrc ]]	&& . "$ktest_dir/ktestrc"
[[ -f /etc/ktestrc ]]		&& . /etc/ktestrc
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"

# defaults:
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"
[[ -f $HOME/.ktest/root ]]	&& ktest_root_image="$HOME/.ktest/root"

# args:

ktest_args="i:s:d:a:p:ISFLvx"
parse_ktest_arg()
{
    local arg=$1

    case $arg in
	i)
	    ktest_root_image=$OPTARG
	    ;;
	s)
	    ktest_tmp=$OPTARG
	    ktest_no_cleanup_tmpdir=1
	    ;;
	d)
	    ktest_vmdir=$OPTARG
	    ;;
	a)
	    ktest_arch=$OPTARG
	    ;;
	p)
	    ktest_priority=$OPTARG
	    ;;
	I)
	    ktest_interactive=1
	    ;;
	S)
	    ktest_exit_on_success=1
	    ;;
	F)
	    ktest_failfast=1
	    ;;
	L)
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
    parse_arch "$ktest_arch"

    checkdep $QEMU_BIN $QEMU_PACKAGE

    if [[ -z $ktest_root_image ]]; then
	ktest_root_image=/var/lib/ktest/root.$DEBIAN_ARCH
    fi
    if [[ -z $ktest_out ]]; then
	ktest_out=./ktest-out
    fi

    ktest_out=$(readlink -f "$ktest_out")

    if [[ -z $ktest_vmdir ]]; then
	ktest_vmdir=$ktest_out/vm
    fi

    if [[ $ktest_interactive = 1 ]]; then
	ktest_kgdb=1
    else
	ktest_crashdump=1
    fi
}

ktest_usage_opts()
{
    echo "      -x          bash debug statements"
    echo "      -h          display this help and exit"
}

ktest_usage_run_opts()
{
    echo "      -p <num>    hint for test duration (higher is longer, default is 0)"
    echo "      -a <arch>   architecture"
    echo "      -i <image>  ktest root image"
    echo "      -s <dir>    directory for scratch drives"
    echo "      -o <dir>    test output directory; defaults to ktest-out"
    echo "      -I          interactive mode - don't shut down VM automatically"
    echo "      -S          exit on test success"
    echo "      -F          failfast - stop after first test failure"
    echo "      -L          run all tests in infinite loop until failure"
    echo "      -v          verbose mode"
}

ktest_usage_cmds()
{
    echo "  boot            Boot a VM without running anything"
    echo "  run <test>      Run a kernel test"
    echo "  ssh             Login as root"
    echo "  gdb             Connect to qemu's gdb interface"
    echo "  kgdb            Connect to kgdb"
    echo "  mon             Connect to qemu monitor"
    echo "  sysrq <key>     Send magic sysrq key via monitor"
}

ktest_usage_post()
{
    echo "For kgdb to be enabled, either -I or -S must be specified"
}

# subcommands:

ktest_run_cleanup()
{
    kill -9 -- -$$ >/dev/null 2>&1 || true
    cleanup_tmpdir
}

ktest_boot()
{
    ktest_interactive=1
    ktest_kgdb=1

    ktest_run "$ktest_dir/boot.ktest" "$@"
}

ktest_ssh()
{
    sock=$ktest_vmdir/net
    ip="10.0.2.2"

    (cd "$ktest_dir/lib"; make lwip-connect) > /dev/null

    exec ssh -t -F /dev/null						\
	-o CheckHostIP=no						\
	-o StrictHostKeyChecking=no					\
	-o UserKnownHostsFile=/dev/null					\
	-o NoHostAuthenticationForLocalhost=yes				\
	-o ServerAliveInterval=2					\
	-o ControlMaster=auto						\
	-o ControlPath="$ktest_vmdir/controlmaster"			\
	-o ControlPersist=yes						\
	-o ProxyCommand="$ktest_dir/lib/lwip-connect $sock $ip 22"	\
	root@127.0.0.1 "$@"
}

ktest_gdb()
{
    if [[ -z $ktest_kernel_binary ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    exec gdb -ex "set remote interrupt-on-connect"			\
	     -ex "target remote | socat UNIX-CONNECT:$ktest_vmdir/vm-gdb -"\
	     "$ktest_kernel_binary/vmlinux"
}

ktest_kgdb()
{
    if [[ -z $ktest_kernel_binary ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    ktest_sysrq g

    exec gdb -ex "set remote interrupt-on-connect"			\
	     -ex "target remote | socat UNIX-CONNECT:$ktest_vmdir/vm-kgdb -"\
	     "$ktest_kernel_binary/vmlinux"
}

ktest_mon()
{
    exec socat UNIX-CONNECT:"$ktest_vmdir/vm-mon" STDIO
    exec nc "$ktest_vmdir/vm-0-mon"
    #exec minicom -D "unix#$ktest_vmdir/vm-0-mon"
}

ktest_sysrq()
{
    local key=$1

    echo sendkey alt-sysrq-$key | socat - "UNIX-CONNECT:$ktest_vmdir/vm-mon"
}

start_networking()
{
    local net="$ktest_tmp/net"

    mkfifo "$ktest_tmp/vde_input"
    tail -f "$ktest_tmp/vde_input" |vde_switch -sock "$net" >/dev/null 2>&1 &

    while [[ ! -e "$net" ]]; do
	sleep 0.1
    done
    slirpvde --sock "$net" --dhcp=10.0.2.2 --host 10.0.2.1/24 >/dev/null 2>&1 &
}

start_vm()
{
    if [[ -z $ktest_kernel_binary ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    if [[ ! -f $ktest_root_image ]]; then
	echo "VM root filesystem not found, use vm_create_image to create one"
	exit 1
    fi

    # upper case vars aren't exported to vm:
    local home=$HOME

    get_tmpdir
    trap 'ktest_run_cleanup' EXIT

    mkdir -p "$ktest_out"
    rm -f "$ktest_out/vm"
    ln -s "$ktest_tmp" "$ktest_out/vm"

    start_networking

    local kernelargs=()
    kernelargs+=(console=hvc0)
    kernelargs+=(root=/dev/sda rw log_buf_len=8M)
    kernelargs+=("ktest.dir=$ktest_dir")
    kernelargs+=("ktest.env=$ktest_tmp/env")
    [[ $ktest_kgdb = 1 ]]	&& kernelargs+=(kgdboc=ttyS0,115200)
    [[ $ktest_verbose = 0 ]]	&& kernelargs+=(quiet systemd.show_status=0)
    [[ $ktest_crashdump = 1 ]]	&& kernelargs+=(crashkernel=128M)

    kernelargs+=("${ktest_kernel_append[@]}")

    local qemu_cmd=("$QEMU_BIN" -nodefaults -nographic)

    case $ktest_arch in
	x86|x86_64)
	    qemu_cmd+=(-cpu host -machine accel=kvm)
	    ;;
	mips)
	    qemu_cmd+=(-cpu 24Kf -machine malta)
	    ktest_cpus=1
	    ;;
	mips64)
	    qemu_cmd+=(-cpu MIPS64R2-generic -machine malta)
	    ;;
    esac

    qemu_cmd+=(								\
	-m		"$ktest_mem"					\
	-smp		"$ktest_cpus"					\
	-kernel		"$ktest_kernel_binary/vmlinuz"			\
	-append		"$(join_by " " ${kernelargs[@]})"		\
	-device		virtio-serial					\
	-chardev	stdio,id=console				\
	-device		virtconsole,chardev=console			\
	-serial		"unix:$ktest_tmp/vm-kgdb,server,nowait"		\
	-monitor	"unix:$ktest_tmp/vm-mon,server,nowait"		\
	-gdb		"unix:$ktest_tmp/vm-gdb,server,nowait"		\
	-device		virtio-rng-pci					\
	-net		nic,model=virtio,macaddr=de:ad:be:ef:00:00	\
	-net		vde,sock="$ktest_tmp/net"			\
	-virtfs		local,path=/,mount_tag=host,security_model=none	\
	-device		$ktest_storage_bus,id=hba			\
    )

    local disknr=0

    qemu_disk()
    {
	qemu_cmd+=(-drive if=none,format=raw,id=disk$disknr,"$1")
	case $ktest_storage_bus in
	    virtio-scsi-pci)
		qemu_cmd+=(-device scsi-hd,bus=hba.0,drive=disk$disknr)
		;;
	    ahci|piix4-ide)
		qemu_cmd+=(-device ide-hd,bus=hba.$disknr,drive=disk$disknr)
		;;
	esac
	disknr=$((disknr + 1))
    }

    qemu_disk file="$ktest_root_image",snapshot=on

    for size in "${ktest_scratch_devs[@]}"; do
	local file="$ktest_tmp/dev-$disknr"

	fallocate -l "$size" "$file"
	qemu_disk file="$file",cache=unsafe
    done

    set|grep -vE '^[A-Z]' > "$ktest_tmp/env"

    set +o errexit

    if [[ $ktest_interactive = 1 ]]; then
	"${qemu_cmd[@]}"
    elif [[ $ktest_exit_on_success = 1 ]]; then
	"${qemu_cmd[@]}"|sed -u -e '/TEST SUCCESS/ { p; Q7 }'
    else
	timeout --foreground "$((60 + ktest_timeout))" "${qemu_cmd[@]}"|
	    $ktest_dir/lib/catch_test_success.awk
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
