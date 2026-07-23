#!/bin/bash
#
# Shared pieces for the memory-pressure torture tests: an unkillable
# native-speed memory eater and swap/memory telemetry.
#
# The eater details are load-bearing (established by ablation against a
# reference qemu harness):
#   - MAP_PRIVATE anonymous, page-touch storm at C speed — reclaim
#     deadlocks form in the initial swap-out/writeback burst;
#   - oom_score_adj -1000 — an OOM-killable eater gets reaped, the
#     pressure collapses and the system recovers;
#   - continuous re-touching so nothing ages out.
# Size it ~RAM+300M to also crush the file LRU: on a disk-rooted VM,
# reclaim otherwise escapes by dropping clean file pages instead of
# pressuring the filesystem under test.

build_eater()
{
    cat > /tmp/eater.c << 'CEOF'
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv)
{
	size_t total = (size_t) atol(argv[1]) << 20;
	int duration = atoi(argv[2]);

	FILE *f = fopen("/proc/self/oom_score_adj", "w");
	if (f) {
		fputs("-1000", f);
		fclose(f);
	}

	unsigned char *p = mmap(NULL, total, PROT_READ|PROT_WRITE,
				MAP_PRIVATE|MAP_ANONYMOUS, -1, 0);
	if (p == MAP_FAILED) {
		perror("mmap");
		return 1;
	}

	for (size_t i = 0; i < total; i += 4096)
		p[i] = 0xAA;
	printf("eater: touched %s MB\n", argv[1]);
	fflush(stdout);

	volatile unsigned char sink = 0;
	time_t deadline = time(NULL) + duration;
	while (time(NULL) < deadline) {
		for (size_t i = 0; i < total; i += 4096 * 64)
			sink ^= p[i];
		sleep(1);
	}
	return 0;
}
CEOF
    gcc -O2 -o /tmp/eater /tmp/eater.c
}

# Default eater size: enough to fill swap (if any) and crush the file LRU.
eater_default_mb()
{
    awk '/MemTotal/ {print int($2 / 1024) + 300}' /proc/meminfo
}

# Squeeze size for swapless tests: anon memory can't be reclaimed at all
# without swap, so an eater above RAM just OOM-cascades within seconds —
# physics, not a filesystem verdict.  ~70% of RAM squeezes reclaim onto
# the filesystem's dirty/clean pages while staying survivable.
eater_squeeze_mb()
{
    awk '/MemTotal/ {print int($2 * 70 / 100 / 1024)}' /proc/meminfo
}

# Sacrificial balloon: a killable, eagerly-OOM-targeted memory hog in a
# respawn loop.  With only the unkillable eater and a few tiny daemons,
# the OOM killer can run out of victims while swap writeback is still
# absorbing the initial burst, and the mm layer panics ("System is
# deadlocked on memory") — a race that can kill even a correct kernel.
# The balloon guarantees a victim, removing that mm-level failure mode
# from both sides of a red/green comparison; the filesystem-level
# deadlock remains the only discriminator.
start_balloon()
{
    local mb=${1:-150}
    (
	# ktest's prelude runs set -eE with an ERR trap; subshells inherit
	# the trap even under set +e, so the loop would die at the first
	# OOM kill of the balloon (exit 137) instead of respawning.
	trap - ERR
	set +e
	while true; do
	    /tmp/eater_balloon "$mb" 3600 2>/dev/null
	    sleep 2
	done
    ) &
    balloon_loop_pid=$!
}

stop_balloon()
{
    [[ -n ${balloon_loop_pid:-} ]] || return 0
    kill $balloon_loop_pid 2>/dev/null || true
    wait $balloon_loop_pid 2>/dev/null || true
    pkill --full "^/tmp/eater_balloon" 2>/dev/null || true
    balloon_loop_pid=
}

build_balloon()
{
    # Same toucher as the eater, but OOM-preferred instead of exempt.
    sed 's/-1000/1000/' /tmp/eater.c > /tmp/eater_balloon.c
    gcc -O2 -o /tmp/eater_balloon /tmp/eater_balloon.c
}

# Memory/swap telemetry, one line per 15s: proof the pressure actually
# materialized (a passing run with untouched swap/dirty proves nothing).
start_mem_telemetry()
{
    (
	trap - ERR
	set +e
	while true; do
	    grep -E "SwapFree|MemFree|Dirty|Writeback:" /proc/meminfo | tr '\n' ' '
	    echo
	    sleep 15
	done
    ) &
    mem_telemetry_pid=$!
}

stop_mem_telemetry()
{
    [[ -n ${mem_telemetry_pid:-} ]] || return 0
    kill $mem_telemetry_pid 2>/dev/null || true
    wait $mem_telemetry_pid 2>/dev/null || true
    mem_telemetry_pid=
}

# hung_task fast-fail for the pressure phase only — swapoff/teardown
# legitimately sit in D state for a long time under load.
hung_task_panic_on()
{
    echo 30 > /proc/sys/kernel/hung_task_timeout_secs
    echo 1  > /proc/sys/kernel/hung_task_panic
}

hung_task_panic_off()
{
    echo 0 > /proc/sys/kernel/hung_task_panic
}
