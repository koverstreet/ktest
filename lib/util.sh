
. "$ktest_dir/lib/common.sh"

ktest_no_cleanup_tmpdir=""
ktest_tmp=${ktest_tmp:-""}

ktest_exit()
{
    local children=$(jobs -rp)
    if [[ -n $children ]]; then
	kill -9 $children >& /dev/null
	wait $(jobs -rp) >& /dev/null
    fi

    [[ -n $ktest_tmp && -z $ktest_no_cleanup_tmpdir ]] && rm -rf "$ktest_tmp"
}

trap ktest_exit EXIT

get_tmpdir()
{
    if [[ -z $ktest_tmp ]]; then
	ktest_tmp=$(mktemp --tmpdir -d $(basename "$0")-XXXXXXXXXX)
    fi
}
