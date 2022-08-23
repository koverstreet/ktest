
. "$ktest_dir/lib/common.sh"

check_root_image_exists()
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
