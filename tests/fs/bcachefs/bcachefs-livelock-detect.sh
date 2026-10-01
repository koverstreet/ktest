#!/bin/bash
#
# Automatic livelock detector for bcachefs ktests.
#
# Livelocks — tasks busy-looping without making progress, e.g. an
# unbounded btree-transaction restart loop — slip past every built-in
# kernel detector:
#   - hung_task (CONFIG_DETECT_HUNG_TASK) only fires on TASK_UNINTERRUPTIBLE
#     (D-state) sleep; a restart loop stays runnable.
#   - softlockup / RCU stall only fire on a CPU that stops scheduling; a
#     restart loop reschedules on every iteration.
# So a livelocked bcachefs only fails a ktest via the coarse wall-clock
# watchdog, minutes later, with no diagnosis of where it was stuck.
#
# This detects it directly from bcachefs's own counters.  A *completing*
# transaction advances the journal sequence number; a *restarting* one
# bumps a trans_restart_* counter without completing.  So:
#
#     trans_restart_* climbing  +  journal seq and data I/O flat  =  livelock
#
# because no transaction is finishing.  The journal-seq signal is what
# keeps this robust against tests that deliberately inject restarts
# (BCACHEFS_INJECT_TRANSACTION_RESTARTS): an injected restart still
# retries and completes, so seq keeps advancing — only a real livelock
# holds it flat.
#
# On detection it dumps the evidence the kernel detectors can't — an
# all-CPU backtrace (sysrq-l shows the *runnable* tasks hung_task never
# sees), the blocked-task list (sysrq-w), and the dominant restart
# reasons (which trans_restart_* counter is spinning tells you where) —
# then, by default, panics (sysrq-c) so the harness fails in seconds with
# that evidence in the log instead of eating the watchdog.
#
# Controls (env):
#   BCACHEFS_LIVELOCK_DISABLE=1     turn the detector off
#   BCACHEFS_LIVELOCK_STALL=60      seconds of no-progress-but-restarting
#                                   before declaring a livelock
#   BCACHEFS_LIVELOCK_ACTION=panic  panic (default) | warn (dump + continue)

# Restart signal vs forward-progress signal, summed across every mounted
# bcachefs.  Echoes "<restarts> <progress>", or nothing if no fs is
# mounted yet.
#
# Restarts: sum of the trans_restart_* counters (all TYPE_COUNTER, so
# plain integers).  Progress: the journal sequence number — a *completing*
# transaction advances it, so it is the cleanest liveness signal and,
# being an integer, avoids the human-readable "75.6 MiB" formatting that
# the TYPE_SECTORS data_read/data_write counters carry.
bch2_livelock_sample()
{
    local base found=0 restarts=0 progress=0
    for base in /sys/fs/bcachefs/*; do
	[[ -d $base/counters ]] || continue
	found=1
	local r seq
	r=$(awk '/since mount/ {s += $NF} END {print s + 0}' \
	    "$base"/counters/trans_restart_* 2>/dev/null)
	seq=$(awk -F'\t' '/^seq:/ {print $2 + 0; exit}' \
	    "$base"/internal/journal_debug 2>/dev/null)
	restarts=$((restarts + ${r:-0}))
	progress=$((progress + ${seq:-0}))
    done
    (( found )) && echo "$restarts $progress"
}

bch2_livelock_dump_and_act()
{
    local stall_secs=$1
    echo "BCACHEFS LIVELOCK: ${stall_secs}s of climbing trans_restart with no journal/data progress" | to_kmsg 2>/dev/null || \
	echo "BCACHEFS LIVELOCK: ${stall_secs}s of climbing trans_restart with no journal/data progress"

    echo 1 > /proc/sys/kernel/sysrq 2>/dev/null || true
    # All-CPU backtrace: the runnable, spinning tasks hung_task can't show.
    echo l > /proc/sysrq-trigger 2>/dev/null || true
    # Blocked-task list, for anything that IS in D-state alongside.
    echo w > /proc/sysrq-trigger 2>/dev/null || true

    local base
    for base in /sys/fs/bcachefs/*; do
	[[ -d $base/counters ]] || continue
	echo "=== $base: nonzero trans_restart counters ==="
	local f
	for f in "$base"/counters/trans_restart_*; do
	    [[ -e $f ]] || continue
	    local v
	    v=$(awk '/since mount/ {print $NF; exit}' "$f" 2>/dev/null)
	    (( ${v:-0} > 0 )) && echo "  $(basename "$f"): $v"
	done
    done

    if [[ ${BCACHEFS_LIVELOCK_ACTION:-panic} = panic ]]; then
	sync
	# Panic so the supervisor's panic post-mortem fails the test fast,
	# with the backtrace above already in the console log.
	echo c > /proc/sysrq-trigger
    fi
}

bch2_livelock_watchdog_start()
{
    [[ ${BCACHEFS_LIVELOCK_DISABLE:-} ]] && return 0

    local stall_secs=${BCACHEFS_LIVELOCK_STALL:-60}
    local poll=3
    echo "livelock watchdog: armed (stall=${stall_secs}s, action=${BCACHEFS_LIVELOCK_ACTION:-panic})"
    # A livelocked transaction restarts far more often than the fs
    # completes work.  Per poll interval we flag a "stall" when both:
    #   - restarts climbed by at least MIN_RESTARTS (filters idle/light
    #     periods — a spinning restart loop does tens of thousands/sec,
    #     so even a few seconds clears this easily), and
    #   - restarts dwarf completions: delta_restarts > RATIO * delta_seq
    #     (a completing transaction advances the journal seq; healthy
    #     work restarts far less than it completes, a livelock far more).
    # This catches whole-fs livelocks (journal frozen -> delta_seq 0 ->
    # ratio infinite) AND partial ones (one task spinning while the
    # journal only trickles).  stall_secs of sustained stalls = livelock.
    local min_restarts=${BCACHEFS_LIVELOCK_MIN_RESTARTS:-20000}
    local ratio=${BCACHEFS_LIVELOCK_RATIO:-1000}
    (
	# The prelude runs set -eE with an ERR trap that subshells inherit;
	# drop it so a transient counter-read hiccup can't kill the loop.
	trap - ERR
	set +e
	local need=$(( (stall_secs + poll - 1) / poll ))
	local stalls=0 prev_r=-1 prev_p=-1
	while true; do
	    sleep $poll
	    local sample r p
	    sample=$(bch2_livelock_sample)
	    if [[ -z $sample ]]; then
		stalls=0; prev_r=-1; continue   # no fs mounted yet
	    fi
	    r=${sample% *}
	    p=${sample#* }
	    if (( prev_r >= 0 )); then
		local dr=$(( r - prev_r ))
		local dp=$(( p - prev_p ))
		(( dp < 0 )) && dp=0
		[[ ${BCACHEFS_LIVELOCK_DEBUG:-} ]] && \
		    echo "livelock-watch: d_restarts=$dr d_seq=$dp stalls=$stalls"
		if (( dr >= min_restarts )) && (( dr > ratio * dp )); then
		    stalls=$((stalls + 1))
		else
		    stalls=0
		fi
	    fi
	    prev_r=$r; prev_p=$p
	    if (( stalls >= need )); then
		bch2_livelock_dump_and_act "$stall_secs"
		stalls=0                  # in warn mode, keep watching
	    fi
	done
    ) &
    bch2_livelock_watchdog_pid=$!
}

bch2_livelock_watchdog_stop()
{
    [[ -n ${bch2_livelock_watchdog_pid:-} ]] || return 0
    kill "$bch2_livelock_watchdog_pid" 2>/dev/null || true
    wait "$bch2_livelock_watchdog_pid" 2>/dev/null || true
    bch2_livelock_watchdog_pid=
}

# Per-test hooks: prelude's run_test calls ktest_test_setup/teardown around
# each test_* function if they are defined (with an EXIT trap so teardown
# runs even when a test fails under set -e).  Every bcachefs test that
# sources bcachefs-test-libs.sh thus gets livelock detection for free.
ktest_test_setup()    { bch2_livelock_watchdog_start; }
ktest_test_teardown() { bch2_livelock_watchdog_stop; }
