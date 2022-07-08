
parse_test_deps()
{
    ktest_basename=$(basename -s .ktest "$ktest_test")

    #export ktest_crashdump
    eval $("$ktest_test" deps)

    parse_arch "$ktest_arch"

    if [ -z "$ktest_mem" ]; then
	echo "test must specify config-mem"
	exit 1
    fi

    if [ -z "$ktest_timeout" ]; then
	ktest_timeout=6000
    fi

    ktest_tests=$("$ktest_test" list-tests)

    if [[ -z $ktest_tests ]]; then
	echo "No tests found"
	echo "TEST FAILED"
	exit 1
    fi

    local t

    # Ensure specified tests exist:
    if [[ -n $ktest_testargs ]]; then
	for t in $ktest_testargs; do
	    if ! echo "$ktest_tests"|grep -wq "$t"; then
		echo "Test $t not found"
		exit 1
	    fi
	done

	ktest_tests="$ktest_testargs"
    fi

    # Mark tests not run:
    rm -rf "$ktest_out/out"
    mkdir  "$ktest_out/out"
    for t in $ktest_tests; do
	t=$(echo "$t"|tr / .)

	mkdir -p $ktest_out/out/$ktest_basename.$t
	echo "========= NOT STARTED" > $ktest_out/out/$ktest_basename.$t/status
    done
}
