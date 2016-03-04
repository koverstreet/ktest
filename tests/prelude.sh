

if [[ $KERNEL_ARCH = x86 ]]; then
    require-kernel-config SMP
    require-kernel-config MCORE2	# optimize for core2
    require-kernel-config IA32_EMULATION
    require-kernel-config IO_DELAY_0XED
fi

if [[ $KERNEL_ARCH = powerpc ]]; then
    require-kernel-config ADVANCED_OPTIONS
fi
# Normal kernel functionality:
require-kernel-config PREEMPT
require-kernel-config NO_HZ
require-kernel-config HZ_100
require-kernel-config HIGH_RES_TIMERS

require-kernel-config SYSVIPC
require-kernel-config CGROUPS
require-kernel-config SLAB
require-kernel-config SWAP		# systemd segfaults if you don't have swap support...
require-kernel-config MODULES
require-kernel-config DEVTMPFS
require-kernel-config DEVTMPFS_MOUNT
require-kernel-config BINFMT_SCRIPT

require-kernel-config PROC_KCORE	# XXX Needed?

# PCI:
require-kernel-config PCI
require-kernel-config VIRTIO_PCI

# Clock:
require-kernel-config RTC_CLASS
require-kernel-config RTC_HCTOSYS
require-kernel-config RTC_DRV_CMOS

# Console:
require-kernel-config SERIAL_8250	# XXX can probably drop
require-kernel-config SERIAL_8250_CONSOLE
require-kernel-config VIRTIO_CONSOLE

# Block devices:
require-kernel-config SCSI
require-kernel-config SCSI_LOWLEVEL	# what's this for?
require-kernel-config SCSI_VIRTIO
require-kernel-config BLK_DEV_SD	# disk support

# Networking
require-kernel-config NET
require-kernel-config PACKET
require-kernel-config UNIX
require-kernel-config INET
require-kernel-config IP_MULTICAST
#require-kernel-config IP_PNP
#require-kernel-config IP_PNP_DHCP
require-kernel-config NETDEVICES
require-kernel-config VIRTIO_NET

# Filesystems:
require-kernel-config TMPFS
require-kernel-config INOTIFY_USER
require-kernel-config CONFIGFS_FS	# systemd

# Root filesystem:
require-kernel-config EXT4_FS
require-kernel-config EXT4_FS_POSIX_ACL

# Tests are passed to VM as an iso image:
require-kernel-config BLK_DEV_SR	# cdrom support
require-kernel-config ISO9660_FS

require-kernel-config NET_9P
require-kernel-config NET_9P_VIRTIO
require-kernel-config NETWORK_FILESYSTEMS
require-kernel-config 9P_FS

# Crash dumps
if [[ $KERNEL_ARCH = x86 ]]; then
    require-kernel-config KEXEC
    require-kernel-config CRASH_DUMP
    require-kernel-config RELOCATABLE
fi

# KGDB:
require-kernel-config KGDB
require-kernel-config KGDB_SERIAL_CONSOLE

# Profiling:
require-kernel-config PROFILING
require-kernel-config JUMP_LABEL

# Tracing
require-kernel-config FTRACE
require-kernel-config FTRACE_SYSCALLS
#require-kernel-config ENABLE_DEFAULT_TRACERS

# Debugging options
require-kernel-config ENABLE_WARN_DEPRECATED
require-kernel-config ENABLE_MUST_CHECK

require-kernel-config MAGIC_SYSRQ
require-kernel-config DEBUG_INFO
require-kernel-config DEBUG_INFO_DWARF4
require-kernel-config GDB_SCRIPTS
require-kernel-config DEBUG_KERNEL
require-kernel-config PANIC_ON_OOPS

# More expensive
#require-kernel-config DYNAMIC_DEBUG

# Expensive
#require-kernel-config DEBUG_LIST
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
