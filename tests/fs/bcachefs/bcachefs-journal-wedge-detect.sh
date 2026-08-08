#!/bin/bash
#
# Journal-wedge detector for bcachefs ktests.
#
# The failure this catches (seen twice on a production desktop, 2026-07-25 and
# 2026-07-26, both fatal):
#
#   bch2_journal_do_writes_locked() only starts the write for a seq once
#   journal_state_seq_count() for that seq reaches zero.  So a single journal
#   reservation held indefinitely on an already-closed buffer pins
#   j->seq_write_started forever.  Once
#
#       atomic64_read(&j->seq) - j->seq_write_started == JOURNAL_STATE_BUF_NR
#
#   (which is 4), every journal_entry_open() returns journal_max_open and the
#   filesystem is finished — no IO error, no device fault, reclaim never even
#   kicked.  The box stays up, but nothing that needs the journal completes.
#
# Why the existing detectors miss it:
#   - livelock-detect keys on trans_restart climbing; here nothing restarts,
#     everything just blocks, and the journal seq goes flat rather than the
#     restart counters going wild.
#   - hung_task does fire eventually, but only after 120s, and it names an
#     arbitrary victim rather than the reservation holder.
#
# The signal, from journal_debug:
#
#     depth = <head seq> - <oldest unwritten seq>
#
# IMPORTANT: a nonzero refcount on the oldest unwritten entry is NORMAL when
# that entry is also the head — that is just the open buffer taking
# reservations.  Verified on a healthy filesystem: head == oldest unwritten,
# refcount 1, depth 0.  Keying on "refcount != 0" alone fires constantly.
# The anomaly is a CLOSED buffer still pinned, i.e. depth >= 1 persisting.
# depth 4 is fatal, so sustained depth >= 2 is the early warning — this fires
# while the filesystem is still healthy, which is the whole point: we do not
# want to have to reproduce a full wedge to get the evidence.
#
# On detection it dumps what actually identifies the culprit:
# debugfs/btree_transactions, which walks every live btree_trans printing a
# full backtrace for each.  With the companion two-line kernel patch teaching
# bch2_btree_trans_to_text() to print trans->journal_res, the holder is named
# outright, and its backtrace answers the question that matters:
#
#   - a real stack   => the reservation is HELD BY A BLOCKED TASK
#   - no live trans holding one => the reservation was LEAKED
#
# Controls (env):
#   BCACHEFS_JWEDGE_DISABLE=1      turn the detector off
#   BCACHEFS_JWEDGE_MINDEPTH=2     depth that counts as anomalous
#   BCACHEFS_JWEDGE_STALL=20       seconds at >= MINDEPTH before declaring
#   BCACHEFS_JWEDGE_ACTION=panic   panic (default) | warn (dump + continue)

# Echo "<head> <oldest_unwritten> <refcount> <depth>" for the first mounted
# bcachefs that has an unwritten entry, or nothing.
#
# journal_debug layout:
#     seq:                       319405175      <- head (first bare "seq:")
#     seq_ondisk:                319405171      <- "^seq:" does not match this
#     ...
#     unwritten entries:
#     seq:                       319405172      <- oldest unwritten
#       refcount:                1
bch2_jwedge_sample()
{
    local base
    for base in /sys/fs/bcachefs/*; do
	[[ -d $base/internal ]] || continue
	awk '
	    !inu && /^seq:/       { head = $2; next }
	    /^unwritten entries:/ { inu = 1; next }
	    inu && /^seq:/        { if (!got) s = $2; next }
	    inu && /^ *refcount:/ { if (!got) { r = $2; got = 1 } ; next }
	    END { if (got) print head, s, r, (head - s) }
	' "$base"/internal/journal_debug 2>/dev/null
	return 0
    done
}

bch2_jwedge_dump_and_act()
{
    local head=$1 seq=$2 ref=$3 depth=$4 stall_secs=$5

    echo "BCACHEFS JOURNAL WEDGE: seq $seq pinned at depth $depth (head $head, refcount $ref) for ${stall_secs}s"

    echo 1 > /proc/sys/kernel/sysrq 2>/dev/null || true

    local base
    for base in /sys/fs/bcachefs/*; do
	[[ -d $base/internal ]] || continue
	local uuid=$(basename "$base")
	echo "=== $uuid: journal_debug ==="
	cat "$base"/internal/journal_debug 2>/dev/null

	# THE artefact: every live btree_trans, each with a backtrace.  With the
	# journal_res line patched into bch2_btree_trans_to_text(), grep this for
	# "journal res: seq $seq" to name the holder directly.
	local dbg=/sys/kernel/debug/bcachefs/$uuid
	if [[ -r $dbg/btree_transactions ]]; then
	    echo "=== $uuid: btree_transactions (holder + backtrace) ==="
	    cat "$dbg"/btree_transactions 2>/dev/null
	else
	    echo "=== $uuid: debugfs unavailable; falling back to sysrq-w only ==="
	fi
	[[ -r $dbg/btree_deadlock ]] && {
	    echo "=== $uuid: btree_deadlock ==="
	    cat "$dbg"/btree_deadlock 2>/dev/null
	}

	echo "=== $uuid: journal_reclaim ==="
	cat "$base"/internal/journal_reclaim 2>/dev/null
	echo "=== $uuid: btree_write_buffer ==="
	cat "$base"/internal/btree_write_buffer 2>/dev/null
	echo "=== $uuid: moving_ctxts ==="
	cat "$base"/internal/moving_ctxts 2>/dev/null
    done

    # Blocked-task stacks: if the holder is blocked rather than leaked, it is here.
    echo w > /proc/sysrq-trigger 2>/dev/null || true

    if [[ ${BCACHEFS_JWEDGE_ACTION:-panic} = panic ]]; then
	sync
	echo c > /proc/sysrq-trigger
    fi
}

bch2_journal_wedge_watchdog_start()
{
    [[ ${BCACHEFS_JWEDGE_DISABLE:-} ]] && return 0

    local stall_secs=${BCACHEFS_JWEDGE_STALL:-20}
    local mindepth=${BCACHEFS_JWEDGE_MINDEPTH:-2}
    local poll=2
    echo "journal-wedge watchdog: armed (mindepth=$mindepth, stall=${stall_secs}s, action=${BCACHEFS_JWEDGE_ACTION:-panic})"
    (
	# Match livelock-detect: the prelude's set -eE ERR trap is inherited by
	# subshells, and a transient read hiccup must not kill the loop.
	trap - ERR
	set +e
	local need=$(( (stall_secs + poll - 1) / poll ))
	local stalls=0 prev_seq=""
	while true; do
	    sleep $poll
	    local sample head seq ref depth
	    sample=$(bch2_jwedge_sample)
	    if [[ -z $sample ]]; then
		stalls=0; prev_seq=""; continue
	    fi
	    set -- $sample
	    head=$1 seq=$2 ref=$3 depth=$4

	    [[ ${BCACHEFS_JWEDGE_DEBUG:-} ]] && \
		echo "jwedge-watch: head=$head oldest=$seq ref=$ref depth=$depth stalls=$stalls"

	    # Only a *stuck* seq counts: if the oldest unwritten seq advances,
	    # the journal is draining normally however deep it momentarily got.
	    if (( depth >= mindepth )) && (( ref != 0 )) && [[ $seq = "$prev_seq" ]]; then
		stalls=$((stalls + 1))
	    else
		stalls=0
	    fi
	    prev_seq=$seq

	    if (( stalls >= need )); then
		bch2_jwedge_dump_and_act "$head" "$seq" "$ref" "$depth" "$stall_secs"
		stalls=0		  # in warn mode, keep watching
	    fi
	done
    ) &
    bch2_journal_wedge_watchdog_pid=$!
}

bch2_journal_wedge_watchdog_stop()
{
    [[ -n ${bch2_journal_wedge_watchdog_pid:-} ]] || return 0
    kill "$bch2_journal_wedge_watchdog_pid" 2>/dev/null || true
    wait "$bch2_journal_wedge_watchdog_pid" 2>/dev/null || true
    bch2_journal_wedge_watchdog_pid=
}

# bcachefs-livelock-detect.sh (sourced by bcachefs-test-libs.sh) already owns
# these hooks; chain onto it rather than clobbering it, so a test that sources
# this file gets both detectors.
ktest_test_setup()
{
    bch2_livelock_watchdog_start
    bch2_journal_wedge_watchdog_start
}

ktest_test_teardown()
{
    bch2_journal_wedge_watchdog_stop
    bch2_livelock_watchdog_stop
}
