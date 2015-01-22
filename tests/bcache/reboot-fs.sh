config-cpus 1

nr_iterations=$((($ktest_priority + 1) * 4))
config-timeout $(($nr_iterations * 45 + $(stress_timeout)))

# If test priority is > 0, also do XFS tests
if [ $ktest_priority -gt 0 ]; then
    N=$((nr_iterations / 2))
else
    N=$((nr_iterations + 1))
fi

main()
{
    setup_tracing 'bcache:*'

    # On the first N reboots, test ext4, then test xfs
    if [ $NR_REBOOTS -lt $N ]; then
	FS=ext4
    else
	FS=xfs
    fi

    # On the first reboot and the Nth reboot, re-format
    # the file system
    if [ $NR_REBOOTS = 0 -o $NR_REBOOTS = $N ]; then
	setup_bcache
	setup_fs $FS
    else
	existing_bcache
	existing_fs $FS
	rm -rf /mnt/$dev/*
    fi

    test_antagonist

    # Right before we end or switch to XFS, shut down bcache
    if [ $NR_REBOOTS = $nr_iterations -o $NR_REBOOTS = $((N - 1)) ]; then
	test_dbench
	test_bonnie
	stop_fs
	test_discard
	stop_bcache
    else
	test_dbench &
	test_bonnie &

	sleep 30
    fi

    # On any iteration but the last, reboot
    if [ $NR_REBOOTS != $nr_iterations ]; then
	do_reboot
    fi
}
