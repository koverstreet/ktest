#!/bin/usr/env bash

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
ktest_out="./ktest-out"	# dir for test output (logs, code coverage, etc.)

ktest_priority=0	# hint for how long test should run
ktest_interactive=false	# if set to true, timeout is ignored completely
                        #       sets with: -I
ktest_exit_on_success=0	# if true, exit on success, not failure or timeout
ktest_failfast=false
ktest_loop=false
ktest_verbose=false	# if false, append quiet to kernel commad line
ktest_crashdump=false
ktest_kgdb=false
ktest_ssh_port=0
ktest_networking=user
ktest_dio=off
ktest_nice=0

checkdep socat
checkdep brotli

# config files:
[[ -f $ktest_dir/ktestrc ]]	&& . "$ktest_dir/ktestrc"
[[ -f /etc/ktestrc ]]		&& . /etc/ktestrc
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"

# defaults:
[[ -f $HOME/.ktestrc ]]		&& . "$HOME/.ktestrc"

# args:

ktest_args="a:o:p:ISFLvxn:N:"
parse_ktest_arg()
{
    local arg=$1

    case $arg in
	a)
	    ktest_arch=$OPTARG
	    ;;
	o)
	    ktest_out=$OPTARG
	    ;;
	p)
	    ktest_priority=$OPTARG
	    ;;
	I)
	    ktest_interactive=true
	    ;;
	S)
	    ktest_exit_on_success=1
	    ;;
	F)
	    ktest_failfast=true
	    ;;
	L)
	    ktest_loop=true
	    ;;
	v)
	    ktest_verbose=true
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
    [ -z ${ktest_arch:+x} ] && ktest_arch=$(uname -m)
    parse_arch "$ktest_arch"

    ktest_out=$(readlink -f "$ktest_out")
    ktest_kernel_binary="$ktest_out/kernel.$ktest_arch"

    if $ktest_interactive; then
	ktest_kgdb=true
    else
	ktest_crashdump=true
    fi

    if [[ $ktest_nice != 0 ]]; then
	renice  --priority $ktest_nice $$ >/dev/null
    fi
}

ktest_usage_opts()
{
    echo "      -a <arch>       architecture"
    echo "      -o <dir>        output directory; defaults to ./ktest-out"
    echo "      -n (user|vde)   Networking type to use"
    echo "      -x              bash debug statements"
    echo "      -h              display this help and exit"
}

ktest_usage_run_opts()
{
    echo "      -p <num>        hint for test duration (higher is longer, default is 0)"
    echo "      -I              interactive mode - don't shut down VM automatically"
    echo "      -S              exit on test success"
    echo "      -F              failfast - stop after first test failure"
    echo "      -L              run all tests in infinite loop until failure"
    echo "      -v              verbose mode"
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
    ktest_interactive=true
    ktest_kgdb=true

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
	     -ex "target remote | socat UNIX-CONNECT:$ktest_out/vm/gdb -"\
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
	     -ex "target remote | socat UNIX-CONNECT:$ktest_out/vm/kgdb -"\
	     "$ktest_kernel_binary/vmlinux"
}

ktest_mon()
{
    exec socat UNIX-CONNECT:"$ktest_out/vm/mon" STDIO
    exec nc "$ktest_out/vm/mon"
}

ktest_sysrq()
{
    local key=$1

    echo sendkey alt-sysrq-$key | socat - "UNIX-CONNECT:$ktest_out/vm/mon"
}

save_env()
{
    set |grep -v "^PATH=" > "$ktest_out/vm/env_tmp"
    readonly_variables="$(readonly | cut -d= -f1 | cut -d' ' -f3)"
    for variable in ${readonly_variables}
    do
	grep -v "${variable}" "$ktest_out/vm/env_tmp" > "$ktest_out/vm/env"
	cp "$ktest_out/vm/env" "$ktest_out/vm/env_tmp"
    done
    sed -i "s/^ ;$//g" "$ktest_out/vm/env"
    rm -rf "$ktest_out/vm/env_tmp"
}

get_unused_port()
{
    # This probably shouldn't be needed, but I was unable to determine which
    # part of the pipeline was returning an error:
    set +o pipefail
    comm -23 --nocheck-order \
	<(seq 10000 65535) \
	<(ss -tan | awk '{print $4}' | cut -d':' -f2 | grep '[0-9]\{1,5\}' | sort -n | uniq) \
	| shuf | head -n1
}

#cross compiling bcachefs is a delicate operation,
#so run it in a separate shell
#for now, we're only building libbcachefs.a,
#which compiles 90% of the C code.
#if the operation completes successfully, a .crossarch is made indicating the cross compile has been completed.
#at the next invocation, if .crossarch is still what it should be, there's no need to recompile again.

try_construct_cross_bcachefs()
(
     cd ${ktest_dir}/tests/bcachefs/bcachefs-tools/
     make clean
     rm -rf rust-src/target/release
     rootpath=${ktest_out}/vm/cross-user
     make CC=${ARCH_TRIPLE}-gcc EXTRA_CFLAGS="-I/${rootpath}/usr/include/ -I/${rootpath}/usr/include/${DEBIAN_INCLUDE_HEADERS}/ -ffile-prefix-map=${rootpath}=/" -j $ktest_cpus libbcachefs.a && echo ${ktest_arch} > .crossarch
     find -name "*.d" -exec sed -i "s/${rootpath//\//\\\/}/\//g" {} \;
)

# try to mount the root image via fuse,
# this way the debian target headers can be included instead of the host OS
# if anything fails, target will need to fall back to target qemu compile,
# which is a lot slower, obviously

premake_cross_bcachefs()
{
     [ "$(cat $ktest_dir/tests/bcachefs/bcachefs-tools/.crossarch)" == "$ktest_arch" ] && return 0;
     which fuse2fs > /dev/null 2>&1 || return -1;
     mkdir $ktest_out/vm/cross-user || return -2;
     fuse2fs -o ro $ktest_root_image $ktest_out/vm/cross-user || return -3;
     try_construct_cross_bcachefs
     umount $ktest_out/vm/cross-user;
     return 0;
}

start_vm()
{
    log_verbose "ktest_arch=$ktest_arch"
    checkdep $QEMU_BIN $QEMU_PACKAGE
    check_root_image_exists

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

    rm -f "$ktest_out/core.*"
    rm -f "$ktest_out/vmcore"
    rm -f "$ktest_out/vm"
    ln -s "$ktest_tmp" "$ktest_out/vm"

    local kernelargs=()

    case $ktest_storage_bus in
	virtio-blk)
	    ktest_root_dev="/dev/vda"
	    ;;
	*)
	    ktest_root_dev="/dev/sda"
	    ;;
    esac

    kernelargs+=(root=$ktest_root_dev rw log_buf_len=8M)
    kernelargs+=(mitigations=off)
    kernelargs+=("ktest.dir=$ktest_dir")
    kernelargs+=(ktest.env=$(readlink -f "$ktest_out/vm/env"))
    $ktest_kgdb		&& kernelargs+=(kgdboc=ttyS0,115200 nokaslr)
    $ktest_verbose	|| kernelargs+=(quiet systemd.show_status=0 systemd.log-target=null)
    $ktest_crashdump	&& kernelargs+=(crashkernel=128M)

    kernelargs+=("${ktest_kernel_append[@]}")

    local qemu_cmd=("$QEMU_BIN" -nodefaults -nographic);
    local accel=kvm;
    local cputype=host;
    if [[ "${CROSS_COMPILE:-0}" == "1" ]]; then
	accel=tcg;
	cputype=max;
        premake_cross_bcachefs
        local err=$?
        [ $err != 0 ] && echo "Error precompiling $err. compiling native in qemu if needed";
    fi
    local pciBus="";
    case $ktest_arch in
	x86|x86_64)
	    qemu_cmd+=(-cpu $cputype -machine type=q35,accel=$accel,nvdimm=on)
	    qemu_network_driver="virtio-net-pci"
	    ;;
	aarch64|arm)
	    qemu_cmd+=(-cpu $cputype -machine type=virt,gic-version=max,accel=$accel)
	    qemu_network_driver="virtio-net-pci"
	    ;;
	ppc64)
	    qemu_cmd+=(-machine ppce500 -cpu e6500 -accel tcg)
	    qemu_network_driver="virtio-net-pci"
	    ;;
	s390x)
	    qemu_cmd+=(-cpu max -machine s390-ccw-virtio -accel tcg)
	    qemu_network_driver="virtio"
	    ;;
	sparc64)
	    qemu_cmd+=(-machine sun4u -accel tcg)
	    ktest_cpus=1; #sparc64 currently supports only 1 cpu
            pciBus=",bus=pciB"
	    qemu_network_driver="virtio-net-pci"
	    ;;
	riscv64)
	    qemu_cmd+=(-machine virt -cpu rv64 -accel tcg)
	    qemu_network_driver="virtio-net-pci"
	    ;;
    esac

    local maxmem=$(awk '/MemTotal/ {printf "%dG\n", $2/1024/1024}' /proc/meminfo 2>/dev/null) || maxmem="1T"
    local memconfig="$ktest_mem,slots=8,maxmem=$maxmem"

    [ $BITS == 32 ] &&  memconfig="3G" && ktest_cpus=$((min($ktest_cpus,4))) #do not be fancy on 32-bit hardware.  if it works, it's fine

    qemu_cmd+=(								\
	-m		"$memconfig"					\
	-smp		"$ktest_cpus"					\
	-kernel		"$ktest_kernel_binary/vmlinuz"			\
	-append		"$(join_by " " ${kernelargs[@]})"		\
	-device		pci-bridge,chassis_nr=2,addr=2,id=vfiob$pciBus	\
	-device		virtio-serial-pci,bus=vfiob,addr=1			\
	-chardev	stdio,id=console				\
	-device		virtconsole,chardev=console			\
	-device		virtio-rng-pci,bus=vfiob,addr=3			\
	-serial		"unix:$ktest_out/vm/kgdb,server,nowait"		\
	-monitor	"unix:$ktest_out/vm/mon,server,nowait"		\
	-gdb		"unix:$ktest_out/vm/gdb,server,nowait"		\
	-fsdev		local,path=/,security_model=none,id=host_dev,multidevs=remap \
	-device		virtio-9p-pci,bus=vfiob,addr=4,fsdev=host_dev,mount_tag=host \
    )
#	-virtfs		local,path=/,mount_tag=host,security_model=none,multidevs=remap	\


    if [[ -f $ktest_kernel_binary/initramfs ]]; then
	qemu_cmd+=(-initrd 	"$ktest_kernel_binary/initramfs")
    fi

    case $ktest_networking in
	user)
	    ktest_ssh_port=$(get_unused_port)
	    echo $ktest_ssh_port > "$ktest_out/vm/ssh_port"
	    qemu_cmd+=( \
		-nic    user,model=${qemu_network_driver},hostfwd=tcp:127.0.0.1:$ktest_ssh_port-:22	\
	    )
	    ;;
	vde)
	    local net="$ktest_out/vm/net"

	    checkdep vde_switch	vde2

	    [[ ! -p "$ktest_out/vm/vde_input" ]] && mkfifo "$ktest_out/vm/vde_input"
	    tail -f "$ktest_out/vm/vde_input" |vde_switch -sock "$net" >& /dev/null &

	    while [[ ! -e "$net" ]]; do
		sleep 0.1
	    done
	    slirpvde --sock "$net" --dhcp=10.0.2.2 --host 10.0.2.1/24 >& /dev/null &
	    qemu_cmd+=( \
		-net		nic,model=virtio,macaddr=de:ad:be:ef:00:00	\
		-net		vde,sock="$ktest_out/vm/net"			\
	    )
	    ;;
	*)
	    echo "Invalid networking type $ktest_networking"
	    exit 1
    esac

    case $ktest_storage_bus in
	virtio-blk)
	    ;;
	*)
	    qemu_cmd+=(-device $ktest_storage_bus$pciBus,id=hba)
	    ;;
    esac

    local disknr=0

    qemu_disk()
    {
	qemu_cmd+=(-drive if=none,format=raw,id=disk$disknr,"$1")
	case $ktest_storage_bus in
	    ahci|piix4-ide)
		qemu_cmd+=(-device ide-hd,bus=hba.$disknr,drive=disk$disknr)
		;;
	    virtio-blk)
		qemu_cmd+=(-device virtio-blk-pci$pciBus,drive=disk$disknr)
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

    for size in "${ktest_scratch_dev_sizes[@]}"; do
	local file="$ktest_out/vm/dev-$disknr"

	truncate -s "$size" "$file"

	qemu_disk file="$file",cache=unsafe
    done

    for size in "${ktest_scratch_slowdevs[@]}"; do
	local file="$ktest_out/vm/dev-$disknr"

	truncate -s "$size" "$file"

	# slow device, 300 kiops and 100MB/s
	qemu_disk file="$file",iops=300,bps=$((100*1024**2))
    done

    for size in "${ktest_pmem_devs[@]}"; do
	local file="$ktest_out/vm/dev-$disknr"

	fallocate -l "$size" "$file"
	qemu_pmem mem-path="$file",size=$size
    done

    [ "$(ulimit)" == "unlimited" ] || ulimit -n 65535
    qemu_cmd+=("${ktest_qemu_append[@]}")

    set +o errexit
    save_env
    "${qemu_cmd[@]}"
}
