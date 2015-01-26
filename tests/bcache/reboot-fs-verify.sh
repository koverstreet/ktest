config-cpus 1

tarball=linux-3.19-rc4.tar.xz

require-make ../../test-data/Makefile ${tarball} sha1.txt

if [ $ktest_priority -gt 0 ]; then
    nr_iterations=6
else
    nr_iterations=3
fi

config-timeout $(($nr_iterations * 300))

main()
{
    setup_tracing 'bcache:*'

    case $((NR_REBOOTS / 3)) in
	0)
	    FS=ext4
	    ;;
	1)
	    FS=xfs
	    ;;
    esac

    case $((NR_REBOOTS % 3)) in
	0)
	    setup_bcache
	    setup_fs $FS

	    test_antagonist

	    cd /mnt/dev/bcache0

	    echo "Unpacking ${tarball}..."
	    tar xfJ /cdrom/${tarball}

	    echo "Verifying checksums..."
	    find linux-3.19-rc4 -type f -exec sha1sum {} \; | sort -k 2 > sha1.txt
	    diff sha1.txt /cdrom/sha1.txt > /dev/null
	    ;;
	1)
	    existing_bcache
	    existing_fs $FS

	    test_antagonist

	    cd /mnt/dev/bcache0

	    echo "Packing ${tarball}..."
	    tar cfJ ${tarball} linux-3.19-rc4

	    echo "Unpacking ${tarball}..."
	    tar xfJ ${tarball}
	    ;;
	2)
	    existing_bcache
	    existing_fs $FS

	    test_antagonist

	    cd /mnt/dev/bcache0

	    echo "Verifying checksums..."
	    find linux-3.19-rc4 -type f -exec sha1sum {} \; | sort -k 2 > sha1.txt
	    diff sha1.txt /cdrom/sha1.txt > /dev/null

	    cd /

	    stop_fs
	    test_discard
	    ;;
    esac

    if [ $((NR_REBOOTS + 1)) != $nr_iterations ]; then
	do_reboot
    fi
}
