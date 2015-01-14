config-cpus 1

nr_iterations=$((($ktest_priority + 1) * 5))
config-timeout $(($nr_iterations * 45 + $(stress_timeout)))

main()
{
    setup_tracing 'bcache:*'

    if [ $NR_REBOOTS = 0 ]; then
	setup_bcache
	setup_fs ext4
    else
	existing_bcache
	existing_fs ext4
	rm -rf /mnt/$dev/*
    fi

    test_antagonist

    if [ $NR_REBOOTS = $nr_iterations ]; then
	test_dbench
	test_bonnie
	stop_fs
	test_discard
	stop_bcache
    else
	test_dbench &
	test_bonnie &

	sleep 30
	do_reboot
    fi
}
