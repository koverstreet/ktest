
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
ktest_out="./ktest_out"	# dir for test output (logs, code coverage, etc.)

ktest_priority=0	# hint for how long test should run
ktest_interactive=0     # if set to 1, timeout is ignored completely
                        #       sets with: -I
ktest_exit_on_success=0	# if true, exit on success, not failure or timeout
ktest_failfast=0
ktest_loop=0
ktest_verbose=0		# if false, append quiet to kernel commad line
ktest_crashdump=0
ktest_kgdb=0
ktest_ssh_port=0
ktest_networking=user
ktest_dio=off
ktest_nice=0

ktest_storage_bus=virtio-scsi-pci

checkdep socat
checkdep qemu-system-x86_64	qemu-system-x86

# config files:
[[ -f $ktest_dir/ktestrc ]]	&& . "$ktest_dir/ktestrc"
[[ -f /etc/ktestrc ]]		&& . /etc/ktestrc
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"

# defaults:
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"
[[ -f $HOME/.ktest/root ]]	&& ktest_root_image="$HOME/.ktest/root"

# args:

ktest_args="i:s:a:p:ISFLvxn:N:"
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
	n)
	    ktest_networking=$OPTARG
	    ;;
	N)
	    ktest_nice=$OPTARG
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

    ktest_out=$(readlink -f "$ktest_out")

    if [[ $ktest_interactive = 1 ]]; then
	ktest_kgdb=1
    else
	ktest_crashdump=1
    fi

    if [[ $ktest_nice != 0 ]]; then
	renice  --priority $ktest_nice $$
    fi
}

ktest_usage_opts()
{
    echo "      -x              bash debug statements"
    echo "      -h              display this help and exit"
}

ktest_usage_run_opts()
{
    echo "      -p <num>        hint for test duration (higher is longer, default is 0)"
    echo "      -a <arch>       architecture"
    echo "      -i <image>      ktest root image"
    echo "      -s <dir>        directory for scratch drives"
    echo "      -o <dir>        test output directory; defaults to ktest-out"
    echo "      -I              interactive mode - don't shut down VM automatically"
    echo "      -S              exit on test success"
    echo "      -F              failfast - stop after first test failure"
    echo "      -L              run all tests in infinite loop until failure"
    echo "      -v              verbose mode"
    echo "      -n (user|vde)   Networking type to use"
    echo "      -N <val>        Nice value for kernel build and VM"
}

ktest_usage_cmds()
{
    echo "  boot                Boot a VM without running anything"
    echo "  run <test>          Run a kernel test"
    echo "  ssh                 Login as root"
    echo "  gdb                 Connect to qemu's gdb interface"
    echo "  kgdb                Connect to kgdb"
    echo "  mon                 Connect to qemu monitor"
    echo "  sysrq <key>         Send magic sysrq key via monitor"
}

ktest_usage_post()
{
    echo "For kgdb to be enabled, either -I or -S must be specified"
}

# subcommands:

ktest_run()
{
    if [[ $# = 0 ]]; then
	echo "$0: missing test"
	usage
	exit 1
    fi

    ktest_test=$1
    shift
    ktest_testargs="$@"

    parse_test_deps "$ktest_test"

    start_vm
}

ktest_boot()
{
    ktest_interactive=1
    ktest_kgdb=1

    ktest_run "$ktest_dir/boot.ktest" "$@"
}

ktest_ssh()
{
    local ssh_cmd=(ssh -t -F /dev/null					\
	    -o CheckHostIP=no						\
	    -o StrictHostKeyChecking=no					\
	    -o UserKnownHostsFile=/dev/null				\
	    -o NoHostAuthenticationForLocalhost=yes			\
	    -o ServerAliveInterval=2					\
	    -o ControlMaster=no					\
	)

    if [[ -f $ktest_out/vm/ssh_port ]]; then
	ktest_ssh_port=$(<$ktest_out/vm/ssh_port)
	ssh_cmd+=(-p $ktest_ssh_port)
    elif [[ -d $ktest_out/vm/net ]]; then
	sock=$ktest_out/vm/net
	ip="10.0.2.2"

	checkdep /usr/include/lwipv6.h liblwipv6-dev
	make -C "$ktest_dir/lib" lwip-connect

	ssh_cmd+=(-o ProxyCommand="$ktest_dir/lib/lwip-connect $sock $ip 22")
    else
	echo "No networking found"
	exit 1
    fi

    exec "${ssh_cmd[@]}" root@localhost "$@"
}

ktest_gdb()
{
    if [[ -z $ktest_kernel_binary ]]; then
	echo "Required parameter -k missing: kernel"
	exit 1
    fi

    exec gdb -ex "set remote interrupt-on-connect"			\
	     -ex "target remote | socat UNIX-CONNECT:$ktest_out/vm/vm-gdb -"\
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
	     -ex "target remote | socat UNIX-CONNECT:$ktest_out/vm/vm-kgdb -"\
	     "$ktest_kernel_binary/vmlinux"
}

ktest_mon()
{
    exec socat UNIX-CONNECT:"$ktest_out/vm/vm-mon" STDIO
    exec nc "$ktest_out/vm/vm-0-mon"
}

ktest_con()
{
    exec socat UNIX-CONNECT:"$ktest_out/vm/vm-con" STDIO
    exec nc "$ktest_out/vm/vm-0-con"
}

ktest_sysrq()
{
    local key=$1

    echo sendkey alt-sysrq-$key | socat - "UNIX-CONNECT:$ktest_out/vm/vm-mon"
}

start_vm()
{
    make -C "$ktest_dir/lib" qemu-wrapper

    local qemu_cmd=("$ktest_dir/lib/qemu-wrapper")

    if [[ $ktest_interactive = 1 ]]; then
	true
    elif [[ $ktest_exit_on_success = 1 ]]; then
	qemu_cmd+=(-S)
    else
	# Inside the VM, we set a timer and on timeout trigger a crash dump. The
	# timeout here is a backup:
	qemu_cmd+=(-S -F -T $((60 + ktest_timeout)))
    fi
    qemu_cmd+=(--)

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

    mkdir -p "$ktest_out"
    rm -f "$ktest_out/vm"
    ln -s "$ktest_tmp" "$ktest_out/vm"

    local kernelargs=()
    kernelargs+=(mitigations=off)
    kernelargs+=(console=hvc0)
    kernelargs+=(root=/dev/sda rw log_buf_len=8M)
    kernelargs+=("ktest.dir=$ktest_dir")
    kernelargs+=("ktest.env=$ktest_tmp/env")
    [[ $ktest_kgdb = 1 ]]	&& kernelargs+=(kgdboc=ttyS0,115200 nokaslr)
    [[ $ktest_verbose = 0 ]]	&& kernelargs+=(quiet systemd.show_status=0 systemd.log-target=journal)
    [[ $ktest_crashdump = 1 ]]	&& kernelargs+=(crashkernel=128M)

    kernelargs+=("${ktest_kernel_append[@]}")

    qemu_cmd+=("$QEMU_BIN" -nodefaults -nographic)
    case $ktest_arch in
	x86|x86_64)
	    qemu_cmd+=(-cpu host -machine type=q35,accel=kvm,nvdimm=on)
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
	-m		"$ktest_mem,slots=8,maxmem=1T"			\
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
	-virtfs		local,path=/,mount_tag=host,security_model=none	\
	-device		$ktest_storage_bus,id=hba			\
    )

    case $ktest_networking in
	user)
	    ktest_ssh_port=$(get_unused_port)
	    echo $ktest_ssh_port > "$ktest_tmp/ssh_port"

	    qemu_cmd+=( \
		-nic    user,model=virtio,hostfwd=tcp:127.0.0.1:$ktest_ssh_port-:22	\
	    )
	    ;;
	vde)
	    local net="$ktest_tmp/net"

	    checkdep vde_switch	vde2

	    [[ ! -p "$ktest_tmp/vde_input" ]] && mkfifo "$ktest_tmp/vde_input"
	    tail -f "$ktest_tmp/vde_input" |vde_switch -sock "$net" >& /dev/null &

	    while [[ ! -e "$net" ]]; do
		sleep 0.1
	    done
	    slirpvde --sock "$net" --dhcp=10.0.2.2 --host 10.0.2.1/24 >& /dev/null &
	    qemu_cmd+=( \
		-net		nic,model=virtio,macaddr=de:ad:be:ef:00:00	\
		-net		vde,sock="$ktest_tmp/net"			\
	    )
	    ;;
	*)
	    echo "Invalid networking type $ktest_networking"
	    exit 1
    esac

    local disknr=0

    qemu_disk()
    {
	qemu_cmd+=(-drive if=none,format=raw,id=disk$disknr,"$1")
	case $ktest_storage_bus in
	    ahci|piix4-ide)
		qemu_cmd+=(-device ide-hd,bus=hba.$disknr,drive=disk$disknr)
		;;
	    *)
		qemu_cmd+=(-device scsi-hd,bus=hba.0,drive=disk$disknr)
		;;
	esac
	disknr=$((disknr + 1))
    }

    qemu_pmem()
    {
	qemu_cmd+=(-object memory-backend-file,id=mem$disknr,share,"$1",align=128M)
	qemu_cmd+=(-device nvdimm,memdev=mem$disknr,id=nv$disknr,label-size=2M)
	disknr=$((disknr + 1))
    }

    qemu_disk file="$ktest_root_image",snapshot=on

    for file in "${ktest_images[@]}"; do
	qemu_disk file="$file",snapshot=on,cache.no-flush=on,cache.direct=$ktest_dio
    done

    for size in "${ktest_scratch_devs[@]}"; do
	local file="$ktest_tmp/dev-$disknr"

	truncate -s "$size" "$file"

	qemu_disk file="$file",cache=unsafe
    done

    for size in "${ktest_pmem_devs[@]}"; do
	local file="$ktest_tmp/dev-$disknr"

	fallocate -l "$size" "$file"
	qemu_pmem mem-path="$file",size=$size
    done

    set |grep -v "^PATH=" > "$ktest_tmp/env_tmp"
    readonly_variables="$(readonly | cut -d= -f1 | cut -d' ' -f3)"
    for variable in ${readonly_variables}
    do
        grep -v "${variable}" "$ktest_tmp/env_tmp" > "$ktest_tmp/env"
        cp "$ktest_tmp/env" "$ktest_tmp/env_tmp"
    done
    sed -i "s/^ ;$//g" "$ktest_tmp/env"
    rm -rf "$ktest_tmp/env_tmp"

    set +o errexit
    set -o pipefail
    shopt -s lastpipe

    "${qemu_cmd[@]}"|tee "$ktest_out/out"
    exit $?
}
