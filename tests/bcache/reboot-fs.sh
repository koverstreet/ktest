config-cpus 1

nr_iterations=$((($ktest_priority + 1) * 5))
config-timeout $(($nr_iterations * 45 + $(stress_timeout)))

test_main()
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

    run_antagonist

    if [ $NR_REBOOTS = $nr_iterations ]; then
	run_dbench
	run_bonnie
	stop_fs
	discard_all_devices
	stop_bcache
    else
	run_dbench &
	run_bonnie &

	sleep 30
	do_reboot
    fi
}
