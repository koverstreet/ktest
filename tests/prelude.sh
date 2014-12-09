
# Normal kernel functionality:

require-kernel-config EXT4_FS
require-kernel-config EXT4_USE_FOR_EXT23
require-kernel-config EXT4_FS_POSIX_ACL
require-kernel-config TMPFS
require-kernel-config INOTIFY_USER

require-kernel-config SCSI
require-kernel-config BLK_DEV_SD # disk support
require-kernel-config BLK_DEV_SR # cdrom support

# systemd segfaults if you don't have swap support...
require-kernel-config SWAP

# shouldn't need this enabled, but systemd also complains
require-kernel-config CONFIGFS_FS

# KVM drivers/ktest functionality:
require-kernel-config VIRTIO_PCI

require-kernel-config VIRTIO_CONSOLE

require-kernel-config NETDEVICES
require-kernel-config VIRTIO_NET
require-kernel-config NET_9P
require-kernel-config NET_9P_VIRTIO
require-kernel-config NETWORK_FILESYSTEMS
require-kernel-config 9P_FS

require-kernel-config SCSI_LOWLEVEL # what's this for?
require-kernel-config SCSI_VIRTIO

# tests are passed to VM as an iso image:
require-kernel-config ISO9660_FS

# Crash dumps
require-kernel-config KEXEC
require-kernel-config CRASH_DUMP
require-kernel-config RELOCATABLE

# KGDB:
require-kernel-config KGDB
require-kernel-config KGDB_SERIAL_CONSOLE

# Debugging options
require-kernel-config ENABLE_WARN_DEPRECATED
require-kernel-config ENABLE_MUST_CHECK

require-kernel-config MAGIC_SYSRQ
require-kernel-config DEBUG_INFO
require-kernel-config DEBUG_KERNEL
require-kernel-config PANIC_ON_OOPS

require-kernel-config DEBUG_LIST
require-kernel-config DEBUG_ATOMIC_SLEEP
require-kernel-config DEBUG_MUTEXES
require-kernel-config DEBUG_PREEMPT

require-kernel-config DEBUG_SLAB
require-kernel-config DEBUG_SPINLOCK

require-kernel-config LOCKDEP_SUPPORT
require-kernel-config PROVE_LOCKING

require-kernel-config PROVE_RCU
require-kernel-config RCU_CPU_STALL_VERBOSE

# expensive, doesn't catch that much
# require-kernel-config DEBUG_PAGEALLOC

# Tracing
require-kernel-config DYNAMIC_DEBUG
require-kernel-config FTRACE
require-kernel-config ENABLE_DEFAULT_TRACERS


