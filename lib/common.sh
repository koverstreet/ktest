
log_verbose()
{
    if [[ $ktest_verbose != 0 ]]; then
	echo "$@"
    fi
}

run_quiet()
{
    local msg=$1
    shift

    if [[ $ktest_verbose = 0 ]]; then
	if [[ -n $msg ]]; then
	    echo -n "$msg... "
	fi

	get_tmpdir
	local out="$ktest_tmp/out-$msg"

	set +e
	(set -e; "$@") > "$out" 2>&1
	local ret=$?
	set -e

	if [[ $ret != 0 ]]; then
	    echo
	    cat "$out"
	    exit 1
	fi

	if [[ -n $msg ]]; then
	    echo done
	fi
    else
	if [[ -n $msg ]]; then
	    echo "$msg:"
	fi
	"$@"
    fi
}

join_by()
{
    local IFS="$1"
    shift
    echo "$*"
}
