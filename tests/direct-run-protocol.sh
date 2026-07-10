#!/usr/bin/env bash

set -o errexit
set -o nounset
set -o pipefail

ROOT=$(dirname "$(readlink -f "$0")")/..

fail()
{
    echo "FAIL: $*" >&2
    exit 1
}

assert_status()
{
    local expected=$1
    local actual

    actual=$(<"$ktest_out/status")
    [[ $actual = "$expected" ]] || fail "status is '$actual', expected '$expected'"
}

assert_trace()
{
    local expected=$1
    local actual

    actual=$(<"$trace")
    [[ $actual = "$expected" ]] || fail "trace was: $actual"
}

run_guest()
{
    local mode=$1 ret=$2

    ktest_interactive=$mode
    trace="$ktest_out/trace"
    : > "$trace"

    # The test double records the guest-visible protocol in one ordered stream.
    ktest_write_result()
    {
        printf '%s\n' "$1" > "$ktest_out/status"
        printf 'status:%s\n' "$1" >> "$trace"
    }
    sync()
    {
        echo sync >> "$trace"
    }
    poweroff()
    {
        echo poweroff >> "$trace"
    }

    set +o errexit
    ktest_finish_guest "$ret" >> "$trace"
    local got=$?
    set -o errexit
    [[ $got = "$ret" ]] || fail "guest returned $got, expected $ret"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ktest_out="$tmp/out"
mkdir -p "$ktest_out"

. "$ROOT/lib/test-result.sh"

# This is the value start_vm creates before qemu is launched, so any failure
# before testrunner starts is a failure rather than qemu's normal shutdown.
ktest_write_result "TEST FAILED"
assert_status "TEST FAILED"

# A noninteractive success is durable before the terminal marker and powers
# off. This is the direct-run path; it deliberately does not use supervisor.
run_guest false 0
assert_status "TEST SUCCESS"
assert_trace $'status:TEST SUCCESS\nsync\nTEST SUCCESS\npoweroff'

# A test failure follows exactly the same shutdown protocol but returns failure.
run_guest false 1
assert_status "TEST FAILED"
assert_trace $'status:TEST FAILED\nsync\nTEST FAILED\npoweroff'

# Host-side initialization covers failures before the guest runner reaches its
# exit trap (bad boot, failed setup, or crashdump collection).
ktest_write_result "TEST FAILED"
assert_status "TEST FAILED"
if ktest_finish_vm 0; then
    fail "early failure was reported as success"
fi

# Interactive runs retain their VM after publishing the result.
run_guest true 0
assert_status "TEST SUCCESS"
assert_trace $'status:TEST SUCCESS\nsync\nTEST SUCCESS'
ktest_finish_vm 0 || fail "guest success was not returned to the host"

# A nonzero qemu exit remains nonzero even if an old/success status exists.
if ktest_finish_vm 37; then
    fail "qemu failure was discarded"
else
    got=$?
    [[ $got = 37 ]] || fail "qemu failure became $got"
fi

# Intentional sysrq reboot and crashdump collection do not publish success:
# both retain the pre-VM failure default until a later normal boot finishes.
ktest_write_result "TEST FAILED"
if ktest_finish_vm 0; then
    fail "reboot/crashdump default failure was reported as success"
fi

# The intentional paths bypass the terminal trap: reboot stays a real sysrq
# reboot, while crashdump collection powers off without publishing success.
grep -F 'echo b > /proc/sysrq-trigger' "$ROOT/lib/testrunner" > /dev/null ||
    fail "sysrq reboot path changed"
crashdump_block=$(sed -n '/if \[\[ -s \/proc\/vmcore \]\]; then/,/^fi$/p' "$ROOT/lib/testrunner")
case $crashdump_block in
    *'trap - EXIT'*'poweroff'*)
        ;;
    *)
        fail "crashdump no longer bypasses the terminal result trap"
        ;;
esac

echo "direct-run protocol: PASS"
