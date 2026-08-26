#!/usr/bin/env bash
#
# Direct-run result protocol shared by the host launcher and guest runner.
#
# The status file is the durable verdict: qemu normally exits successfully for
# both a guest poweroff after success and one after a test failure. Keep it
# separate from console output so the host can distinguish those cases.

: "${ktest_out:?ktest_out must be set before loading test-result.sh}"

ktest_status_file()
{
    echo "$ktest_out/status"
}

ktest_write_result()
{
    local result=$1

    case $result in
        "TEST SUCCESS"|"TEST FAILED")
            ;;
        *)
            echo "invalid ktest result: $result" >&2
            return 1
            ;;
    esac

    printf '%s\n' "$result" > "$(ktest_status_file)"
}

ktest_result_code()
{
    local result

    [[ -r $(ktest_status_file) ]] || return 1
    IFS= read -r result < "$(ktest_status_file)" || return 1

    [[ $result = "TEST SUCCESS" ]]
}

ktest_finish_vm()
{
    local qemu_ret=$1

    # Do not turn a qemu failure into success merely because a stale or
    # partially-written guest status says success. Conversely, a normal guest
    # poweroff is a test failure unless it explicitly recorded TEST SUCCESS.
    [[ $qemu_ret = 0 ]] || return "$qemu_ret"
    ktest_result_code
}

ktest_finish_guest()
{
    local ret=$1
    local result="TEST FAILED"

    [[ $ret = 0 ]] && result="TEST SUCCESS"

    # The host polls this through virtiofs. Publish and flush the durable
    # verdict before printing the terminal marker, then power off direct
    # noninteractive runs. Interactive sessions retain the VM for ssh/kgdb.
    ktest_write_result "$result"
    sync
    echo "$result"

    if [[ ${ktest_interactive:-false} != true ]]; then
        poweroff
    fi

    return "$ret"
}
