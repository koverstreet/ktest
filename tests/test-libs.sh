#!/bin/bash
#
# Library with some functions for writing block layer tests using the
# ktest framework.
#

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/prelude.sh
. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/kconfig.sh

config-mem 8G

(($ktest_cpus > 16)) && ktest_cpus=16

# Usage:
# setup_tracing tracepoint_glob
setup_tracing()
{
    local t=/sys/kernel/tracing

    echo		> "$t"/trace
    echo 8192		> "$t"/buffer_size_kb
    echo $@		> "$t"/set_event
    echo trace_printk	> "$t"/trace_options
    #echo stacktrace	> "$t"/trace_options
    echo 1		> "$t"/options/overwrite
    echo 1		> "$t"/tracing_on

    #echo 1		> /proc/sys/kernel/ftrace_dump_on_oops
}

# Fault injection

set_faults()
{
    f=/sys/kernel/debug/dynamic_fault/control

    if [[ -f $f ]]; then
	echo "$@" > $f
    fi
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

call_base_test()
{
    fname=$(basename ${BASH_SOURCE[2]})
    fname=${fname#$1-}
    shift

    . $(dirname $(readlink -e ${BASH_SOURCE[2]}))/$fname
}
