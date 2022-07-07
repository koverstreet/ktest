#
# Library with some functions for writing block layer tests using the
# ktest framework.
#

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/prelude.sh
. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/kconfig.sh

config-mem 2G

# Usage:
# setup_tracing tracepoint_glob
setup_tracing()
{
    echo > /sys/kernel/debug/tracing/trace
    echo 4096 > /sys/kernel/debug/tracing/buffer_size_kb
    echo $@ > /sys/kernel/debug/tracing/set_event
    echo trace_printk > /sys/kernel/debug/tracing/trace_options
    echo 1 > /proc/sys/kernel/ftrace_dump_on_oops
    echo 1 > /sys/kernel/debug/tracing/options/overwrite
    echo 1 > /sys/kernel/debug/tracing/tracing_on
}

# Fault injection

set_faults()
{
    f=/sys/kernel/debug/dynamic_fault/control

    [[ -f $f ]] && echo "$@" > $f
}

enable_memory_faults()
{
    set_faults "class memory frequency 100"
}

disable_memory_faults()
{
    set_faults "class memory disable"
}

enable_race_faults()
{
    set_faults "class race frequency 1000"
}

disable_race_faults()
{
    set_faults "class race disable"
}

enable_faults()
{
    enable_memory_faults
    enable_race_faults
}

disable_faults()
{
    disable_race_faults
    disable_memory_faults
}

# Generic test antagonists

antagonist_sync()
{
    while true; do
	sync
	sleep 0.5
    done
}

antagonist_drop_caches()
{
    echo 4 > /proc/sys/vm/drop_caches

    while true; do
	echo 3 > /proc/sys/vm/drop_caches
	sleep 5
    done
}

stress_timeout()
{
    echo $((($ktest_priority + 3) * 600))
}
