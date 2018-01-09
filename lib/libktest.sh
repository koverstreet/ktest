
. "$ktest_dir/lib/util.sh"
. "$ktest_dir/lib/parse-test.sh"

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
	    ktest_tmp=$OPTARG
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
    [[ -z $ktest_out ]]		&& ktest_out=./ktest-out

    ktest_kernel=$(readlink -f "$ktest_kernel")
    ktest_out=$(readlink -f "$ktest_out")
}

ktest_run_cleanup()
{
    rm -rf "$ktest_tmp"
    kill -9 -- -$$
}

ktest_run()
{
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

    local ktest_crashdump=0
    [[ $ktest_interactive = 0 ]]	&& ktest_crashdump=1

    get_tmpdir
    trap 'ktest_run_cleanup' EXIT
    echo "$ktest_tmp" > "$ktest_idfile"

    BUILD_DEPS=1
    parse_test_deps "$ktest_test"

    mkdir -p "$ktest_out"

    net="$ktest_tmp/net"
    mkfifo "$ktest_tmp/vde_input"
    tail -f "$ktest_tmp/vde_input" |vde_switch -sock "$net" >/dev/null 2>&1 &

    while [[ ! -e "$net" ]]; do
	sleep 0.1
    done
    slirpvde --sock "$net" "--dhcp=10.0.2.2" "--host" "10.0.2.1/24" >/dev/null 2>&1 &

    kernelargs="console=hvc0 root=/dev/sda rw"
    kernelargs+=" ktest.dir=$ktest_dir"
    kernelargs+=" ktest.env=$ktest_tmp/env"
    kernelargs+=" log_buf_len=8M"
    [[ $ktest_interactive = 1 ]] && kernelargs+=" kgdboc=ttyS0,115200"
    [[ $ktest_verbose = 0 ]]	&& kernelargs+=" quiet systemd.show_status=0"
    [[ $ktest_crashdump = 1 ]]	&& kernelargs+=" crashkernel=128M"

    kernelargs+="$ktest_kernel_append"

    qemu_cmd=("$QEMU_BIN"						\
	-nodefaults							\
	-nographic							\
	-m		"$ktest_mem"					\
	-smp		"$ktest_cpus"					\
	-kernel		"$ktest_kernel/vmlinuz"				\
	-append		"$kernelargs"					\
	-device		virtio-serial					\
	-chardev	stdio,id=console				\
	-device		virtconsole,chardev=console			\
	-serial		"unix:$ktest_tmp/vm-kgdb,server,nowait"		\
	-monitor	"unix:$ktest_tmp/vm-mon,server,nowait"		\
	-gdb		"unix:$ktest_tmp/vm-gdb,server,nowait"		\
	-device		virtio-rng-pci					\
	-net		nic,model=virtio,macaddr=de:ad:be:ef:00:00	\
	-net		vde,sock="$net"					\
	-virtfs		local,path=/,mount_tag=host,security_model=none	\
	-device		virtio-scsi-pci,id=scsi-hba			\
	-drive		if=none,format=raw,id=disk0,file="$ktest_image",snapshot=on\
	-device		scsi-hd,bus=scsi-hba.0,drive=disk0		\
    )

    case $KERNEL_ARCH in
	x86)
	    qemu_cmd+=(-cpu host -machine accel=kvm)
	    ;;
    esac

    local nr=1
    for size in "${ktest_scratch_devs[@]}"; do
	file="$ktest_tmp/dev-$nr"
	fallocate -l "$size" "$file"

	qemu_cmd+=(							\
	    -drive	if=none,format=raw,id=disk$nr,file="$file",cache=unsafe\
	    -device	scsi-hd,bus=scsi-hba.0,drive=disk$nr)
	nr=$((nr + 1))
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

ktest_boot()
{
    ktest_interactive=1

    ktest_run "$ktest_dir/boot.ktest" "$@"
}

ktest_ssh()
{
    vmdir=$(<$ktest_idfile)
    sock=$vmdir/net
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
	     -ex "target remote | socat UNIX-CONNECT:$vmdir/vm-gdb -"	\
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
	     -ex "target remote | socat UNIX-CONNECT:$vmdir/vm-kgdb -"\
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

    echo sendkey alt-sysrq-$key | socat - "UNIX-CONNECT:$vmdir/vm-mon"
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
