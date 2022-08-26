

have_kvmguest=0
have_virtio=0
have_suspend=0

case $ktest_arch in
    x86)
	require-kernel-config SMP
	require-kernel-config MCORE2	# optimize for core2
	require-kernel-config IO_DELAY_0XED
	require-kernel-config 64BIT=n
	require-kernel-config ACPI	# way slower without it, do not know why
	require-kernel-config UNWINDER_FRAME_POINTER
	require-kernel-config HARDLOCKUP_DETECTOR
	require-kernel-config RTC_DRV_CMOS

	have_kvmguest=1
	have_virtio=1
	have_suspend=1

	require-kernel-append console=hvc0
	;;
    x86_64)
	require-kernel-config SMP
	require-kernel-config MCORE2	# optimize for core2
	require-kernel-config IO_DELAY_0XED
	#require-kernel-config IA32_EMULATION
	require-kernel-config 64BIT
	require-kernel-config ACPI	# way slower without it, do not know why
	require-kernel-config UNWINDER_FRAME_POINTER
	require-kernel-config HARDLOCKUP_DETECTOR
	require-kernel-config RTC_DRV_CMOS

	have_kvmguest=1
	have_virtio=1
	have_suspend=1

	require-kernel-append console=hvc0
	;;
    aarch64)
	require-kernel-config PCI_HOST_GENERIC

	have_virtio=1

	require-kernel-append console=hvc0
	;;
    powerpc)
	require-kernel-config ADVANCED_OPTIONS

	have_kvmguest=1
	have_virtio=1
	have_suspend=1

	require-kernel-append console=hvc0
	;;
    mips)
	require-kernel-config MIPS_MALTA
	require-kernel-config CPU_MIPS${BITS}_R2
	require-kernel-config CPU_BIG_ENDIAN=y
	require-kernel-config CPU_LITTLE_ENDIAN=n
	require-kernel-config 32BIT

	have_virtio=1
	ktest_storage_bus=piix4-ide

	require-kernel-append console=hvc0
	;;
    *)
	echo "Kernel architecture not supported by kconfig.sh"
	exit 1
	;;
esac

# Normal kernel functionality:
#require-kernel-config PREEMPT
#require-kernel-config NO_HZ
#require-kernel-config HZ_100

require-kernel-config LOCALVERSION_AUTO

require-kernel-config HIGH_RES_TIMERS

require-kernel-config SYSVIPC
require-kernel-config CGROUPS
require-kernel-config SWAP		# systemd segfaults if you don't have swap support...
require-kernel-config MODULES,MODULE_UNLOAD
require-kernel-config DEVTMPFS
require-kernel-config DEVTMPFS_MOUNT
require-kernel-config BINFMT_ELF
require-kernel-config BINFMT_SCRIPT

require-kernel-config COMPACTION	# virtfs doesn't do well without it

require-kernel-config PROC_KCORE	# XXX Needed?

require-kernel-config TTY
require-kernel-config VT

# KVM guest support:
if [[ $have_kvmguest = 1 ]]; then
    require-kernel-config HYPERVISOR_GUEST
    require-kernel-config PARAVIRT
    require-kernel-config KVM_GUEST
fi

if [[ $have_virtio = 1 ]]; then
    require-kernel-config VIRTIO_MENU
    require-kernel-config VIRTIO_MMIO
    require-kernel-config VIRTIO_PCI
    require-kernel-config HW_RANDOM_VIRTIO
    require-kernel-config VIRTIO_CONSOLE
    require-kernel-config VIRTIO_NET
    require-kernel-config NET_9P_VIRTIO
    require-kernel-config CONFIG_CRYPTO_DEV_VIRTIO
fi

if [[ $have_suspend = 1 ]]; then
    require-kernel-config PM
    require-kernel-config SUSPEND
    require-kernel-config PM_SLEEP
    require-kernel-config PM_DEBUG
    require-kernel-append no_console_suspend
fi

case $ktest_storage_bus in
    virtio-scsi-pci)
	require-kernel-config SCSI_VIRTIO
	;;
    virtio-blk)
	require-kernel-config VIRTIO_BLK
	;;
    ahci)
	require-kernel-config ATA
	require-kernel-config SATA_AHCI
	;;
    piix4-ide)
	require-kernel-config ATA
	require-kernel-config ATA_SFF
	require-kernel-config ATA_PIIX
	;;
    lsi)
	require-kernel-config SCSI_MPT3SAS
	;;
    *)
	echo "No storage bus selected"
	exit 1
	;;
esac

# PCI:
require-kernel-config PCI

# Rng:
require-kernel-config HW_RANDOM

# Clock:
require-kernel-config RTC_CLASS
require-kernel-config RTC_HCTOSYS

# Console:
require-kernel-config SERIAL_8250	# XXX can probably drop
require-kernel-config SERIAL_8250_CONSOLE

# Block devices:
require-kernel-config SCSI
require-kernel-config SCSI_LOWLEVEL	# what's this for?
require-kernel-config BLK_DEV_SD	# disk support

# Networking
require-kernel-config NET
require-kernel-config PACKET
require-kernel-config UNIX
require-kernel-config INET
require-kernel-config IP_MULTICAST
require-kernel-config NETDEVICES

# Filesystems:
require-kernel-config TMPFS
require-kernel-config INOTIFY_USER
require-kernel-config CONFIGFS_FS	# systemd

# Root filesystem:
require-kernel-config EXT4_FS
require-kernel-config EXT4_FS_POSIX_ACL

require-kernel-config NET_9P
require-kernel-config NETWORK_FILESYSTEMS
require-kernel-config 9P_FS

# Crash dumps
#if [[ $ktest_crashdump = 1 ]]; then
#    require-kernel-config KEXEC
#    require-kernel-config CRASH_DUMP
#    require-kernel-config RELOCATABLE
#fi

# KGDB:
require-kernel-config KGDB
require-kernel-config KGDB_SERIAL_CONSOLE
require-kernel-config VMAP_STACK=n
require-kernel-config RANDOMIZE_BASE=n
require-kernel-config RANDOMIZE_MEMORY=n

# Profiling:
require-kernel-config PROFILING
require-kernel-config JUMP_LABEL

# Tracing
require-kernel-config PERF_EVENTS
require-kernel-config FTRACE
require-kernel-config FTRACE_SYSCALLS
require-kernel-config FUNCTION_TRACER
#require-kernel-config ENABLE_DEFAULT_TRACERS

require-kernel-config PANIC_ON_OOPS
require-kernel-config SOFTLOCKUP_DETECTOR
require-kernel-config DETECT_HUNG_TASK
#require-kernel-config DEFAULT_HUNG_TASK_TIMEOUT=30
require-kernel-config WQ_WATCHDOG

require-kernel-config DEBUG_FS
require-kernel-config MAGIC_SYSRQ
require-kernel-config DEBUG_INFO
require-kernel-config DEBUG_INFO_DWARF4
require-kernel-config GDB_SCRIPTS
require-kernel-config DEBUG_KERNEL
#require-kernel-config DEBUG_RODATA
#require-kernel-config DEBUG_SET_MODULE_RONX

require-kernel-config DEBUG_LIST

# More expensive
#require-kernel-config DYNAMIC_DEBUG

# Expensive
#require-kernel-config DEBUG_ATOMIC_SLEEP
#require-kernel-config DEBUG_MUTEXES
#require-kernel-config DEBUG_PREEMPT

#require-kernel-config DEBUG_SLAB
#require-kernel-config DEBUG_SPINLOCK

#require-kernel-config LOCKDEP_SUPPORT
#require-kernel-config PROVE_LOCKING

#require-kernel-config PROVE_RCU
#require-kernel-config RCU_CPU_STALL_VERBOSE

# expensive, doesn't catch that much
# require-kernel-config DEBUG_PAGEALLOC
