
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

checkdep_arch()
{
    if [[ -z $ktest_root_image ]]; then
	if [[ -f $HOME/.ktest/root.$DEBIAN_ARCH ]]; then
	    ktest_root_image="$HOME/.ktest/root.$DEBIAN_ARCH"
	elif [[ -f /var/lib/ktest/root.$DEBIAN_ARCH ]]; then
	    ktest_root_image=/var/lib/ktest/root.$DEBIAN_ARCH
	else
	    echo "Root image not found in $HOME/.ktest/root.$DEBIAN_ARCH or /var/lib/ktest/root.$DEBIAN_ARCH"
	    echo "Use $ktest_dir/root_image create"
	    exit 1
	fi
    fi
}
