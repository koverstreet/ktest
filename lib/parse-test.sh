
parse_test_deps()
{
    export ktest_crashdump
    export ktest_out

    eval $( "$ktest_test" deps $ktest_testargs )

    parse_arch "$ktest_arch"

    if [ -z "$ktest_mem" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$ktest_timeout" ]; then
	ktest_timeout=6000
    fi

    ktest_tests=$("$ktest_test" list-tests)
    ktest_tests=$(echo $ktest_tests)

    if [[ -z $ktest_tests ]] && ! $ktest_tests_unknown; then
	echo "No tests found"
	echo "TEST FAILED"
	exit 1
    fi

    local t found

    # Ensure specified tests exist. A requested test missing from the file
    # is a stale job matrix - a renamed/removed test still queued from an
    # enumeration against a different revision. Skip it and run the rest,
    # rather than failing the whole batch of unrelated subtests; only bail
    # if none of the requested tests exist.
    if [[ -n $ktest_testargs ]]; then
	if ! $ktest_tests_unknown; then
	    found=""
	    for t in $ktest_testargs; do
		if echo "$ktest_tests"|grep -wq "$t"; then
		    found="$found $t"
		else
		    echo "Test $t not found - skipping (stale job matrix?)"
		fi
	    done
	    if [[ -z $found ]]; then
		echo "none of the requested tests exist in $(basename "$ktest_test")"
		exit 1
	    fi
	    ktest_testargs="$found"
	fi

	ktest_tests="$ktest_testargs"
    fi
}
