# Util code

wait_on_dev()
{
	for i in $@; do
		while [ ! -b "/dev/$i" ]; do
			sleep 0.5
		done
	done
}

# Generic setup

setup_netconsole()
{
	IP=`ifconfig eth0|grep inet|sed -e '/inet/ s/.*inet addr:\([.0-9]*\).*$/\1/'`
	REMOTE=`cat /proc/cmdline |sed -e 's/^.*nfsroot=\([0-9.]*\).*$/\1/'`
	PORT=`echo $IP|sed -e 's/.*\(.\)$/666\1/'`

	mkdir		  /sys/kernel/config/netconsole/1
	echo $IP	> /sys/kernel/config/netconsole/1/local_ip
	echo $REMOTE	> /sys/kernel/config/netconsole/1/remote_ip
	echo $PORT	> /sys/kernel/config/netconsole/1/remote_port
	echo 1		> /sys/kernel/config/netconsole/1/enabled
}

setup_dynamic_debug()
{
	#echo "func btree_read +p"		> /sys/kernel/debug/dynamic_debug/control
	echo "func btree_read_work +p"		> /sys/kernel/debug/dynamic_debug/control

	echo "func btree_insert_recurse +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func btree_gc_recurse +p "	> /sys/kernel/debug/dynamic_debug/control

	#echo "func bch_btree_gc_finish +p "	> /sys/kernel/debug/dynamic_debug/control

	echo "func sync_btree_check +p "	> /sys/kernel/debug/dynamic_debug/control
	#echo "func btree_insert_keys +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func __write_super +p"		> /sys/kernel/debug/dynamic_debug/control
	echo "func register_cache_set +p"	> /sys/kernel/debug/dynamic_debug/control
	echo "func run_cache_set +p"		> /sys/kernel/debug/dynamic_debug/control
	echo "func write_bdev_super +p"		> /sys/kernel/debug/dynamic_debug/control
	echo "func detach_bdev +p"		> /sys/kernel/debug/dynamic_debug/control

	echo "func journal_read_bucket +p"	> /sys/kernel/debug/dynamic_debug/control
	echo "func bch_journal_read +p"		> /sys/kernel/debug/dynamic_debug/control
	echo "func bch_journal_mark +p"		> /sys/kernel/debug/dynamic_debug/control
	#echo "func bch_journal_replay +p"	> /sys/kernel/debug/dynamic_debug/control

	#echo "func btree_cache_insert +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func bch_btree_insert_check_key +p" > /sys/kernel/debug/dynamic_debug/control
	#echo "func cached_dev_cache_miss +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func request_read_done_bh +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func bch_insert_data_loop +p"	> /sys/kernel/debug/dynamic_debug/control

	#echo "func bch_refill_keybuf +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "func bcache_keybuf_next_rescan +p"	> /sys/kernel/debug/dynamic_debug/control
	#echo "file movinggc.c +p"		> /sys/kernel/debug/dynamic_debug/control
	#echo "file super.c +p"			> /sys/kernel/debug/dynamic_debug/control

	#echo "func invalidate_buckets +p"	> /sys/kernel/debug/dynamic_debug/control

	#echo "file request.c +p"		> /sys/kernel/debug/dynamic_debug/control

	t=/sys/kernel/debug/tracing/events/bcache/

	#echo 1 | tee $t/*/enable
	#echo 1 > $t/bcache/bcache_btree_read/enable

#	echo 1 > $t/bcache_btree_cache_cannibalize/enable

#	echo 1 > $t/bcache_btree_gc_coalesce/enable
#	echo 1 > $t/bcache_alloc_invalidate/enable
#	echo 1 > $t/bcache_alloc_fail/enable

#	echo 1 > $t/bcache_journal_full/enable
#	echo 1 > $t/bcache_journal_entry_full/enable
}

setup_md_faulty()
{
	CACHE=md0
	#mdadm -C /dev/md0 -l6 -n4 /dev/vd[bcde]
	#mdadm -A /dev/md0 /dev/vd[bcde]

	#mdadm -B /dev/md0 -l0 -n2 /dev/vdb /dev/vdc

	mdadm -B /dev/md0 -lfaulty -n1 /dev/vda

	mdadm -G /dev/md0 -prp10000
}

prep_mkfs()
{
	for dev in $DEV; do
		echo "mkfs on /dev/$dev"
		mkfs.ext4 -F /dev/$dev || exit $?
		#mkfs.xfs -f /dev/$dev || exit $?
	done
}

prep_mounts()
{
	for dev in $DEV; do
		#echo "fsck on /dev/$dev"
		#fsck -E journal_only /dev/$dev
		#fsck -f -n /dev/$dev || exit $?

		mkdir -p /mnt/$dev
		mount -o errors=panic /dev/$dev /mnt/$dev || exit $?
		#mount /dev/$dev /mnt/$dev || exit $?

		cd /mnt/$dev
		find *|grep -v lost+found|wc -l
		find *|grep -v lost+found|xargs rm -rf
	done
}

# Bcache setup

prep_bcache_devices()
{
	cd /dev

	#if [ -f /sys/fs/register_blk_test ]; then
	#	echo /dev/$CACHE > /sys/fs/register_blk_test
	#	udevadm settle
	#	CACHE=blk_test0
	#fi

	false
	echo /dev/$CACHE> /sys/fs/bcache/register

	if [ $? -ne 0 ]; then
		/cdrom/make-bcache --bucket 64k --block 2k		\
			--discard					\
			--cache_replacement_policy=lru			\
			--writeback 					\
			--cache $CACHE					\
			--bdev $BDEV

		for dev in $CACHE $BDEV; do
			echo /dev/$dev	> /sys/fs/bcache/register
		done

		udevadm settle
		prep_mkfs || exit $?
	else
		# XXX
		UUID=`ls -d /sys/fs/bcache/*-*-*`

		for dev in $BDEV; do
			echo /dev/$dev	> /sys/fs/bcache/register

			dir="/sys/block/$dev/bcache"

			if [ ! -d "$dir/cache" ]; then
				echo "not attached!"
				exit 1

			#	echo "$UUID"	> "$dir/attach"
			fi
		done

		udevadm settle
	fi

	rm -f /root/c
	ln -s /sys/fs/bcache/*-* /root/c
}

prep_flash_dev()
{
	for dev in $DEV; do
		echo 500M > /sys/fs/bcache/*/flash_vol_create
	done

	udevadm settle
}

cache_set_settings()
{
	for dir in `ls -d /sys/fs/bcache/*-*-*`; do
		true
		#echo 0 > $dir/synchronous
		echo panic > $dir/errors

		#echo 0 > $dir/journal_delay_ms
		#echo 1 > $dir/internal/key_merging_disabled
		#echo 1 > $dir/internal/btree_coalescing_disabled
		#echo 1 > $dir/internal/verify

#		echo 1 > $dir/internal/expensive_debug_checks

		echo 0 > $dir/congested_read_threshold_us
		echo 0 > $dir/congested_write_threshold_us

		echo 1 > $dir/internal/copy_gc_enabled
	done
}

cached_dev_settings()
{
	for dir in `ls -d /sys/block/bcache*/bcache`; do
		true
		#echo 128k	> $dir/readahead
		#echo 1		> $dir/writeback_delay
		#echo 0		> $dir/writeback_running
		#echo 0		> $dir/sequential_cutoff
		#echo 1		> $dir/verify
		echo 1		> $dir/bypass_torture_test
	done
}

# Bcache specific tests

test_sysfs()
{
	find -H /sys/fs/bcache/*-*/* -type f -perm -0400 \
		|xargs cat > /dev/null
}

test_fault()
{
	[ -f /sys/kernel/debug/dynamic_fault/control ] || return

	while true; do
		echo "file btree.c +o"		> /sys/kernel/debug/dynamic_fault/control 
		echo "file bset.c +o"		> /sys/kernel/debug/dynamic_fault/control 
		echo "file io.c +o"		> /sys/kernel/debug/dynamic_fault/control 
		echo "file journal.c +o"	> /sys/kernel/debug/dynamic_fault/control 
		echo "file request.c +o"	> /sys/kernel/debug/dynamic_fault/control 
		echo "file util.c +o"		> /sys/kernel/debug/dynamic_fault/control 
		echo "file writeback.c +o"	> /sys/kernel/debug/dynamic_fault/control 
	done
}

test_shrink()
{
	while true; do
		echo 100000 > /sys/fs/bcache/*/internal/prune_cache || return
		sleep 0.5
	done
}

test_stop()
{
	sleep 4
	cd /

	for dev in $DEV; do
		fuser -s -k -M /mnt/$dev
	done

	sleep 2

	for dev in $DEV; do
		umount /mnt/$dev
	done

	#echo 1 > /sys/block/bcache0/bcache/stop
	#echo 1 > /sys/block/bcache1/bcache/stop
	echo 1 > /sys/fs/bcache/reboot
}

test_bcache_test()
{
	for dev in $DEV; do
		file=/mnt/$dev/test

		dd if=/dev/urandom of=$file bs=1M count=512 oflag=direct
		/cdrom/bcache-test -dnscw $file &
	done
}

# Various tests

test_bonnie()
{
	while true; do
		for dev in $DEV; do
#			cd /mnt/$dev
			bonnie -x 100000 -u root -d /mnt/$dev &
		done
		wait
	done
}

test_dbench()
{
	for dev in $DEV; do
		dbench -S -t 100000 2 -D /mnt/$dev &
	done
}

test_fio()
{
	echo "Starting fio"
	for dev in $DEV; do
		#cd /mnt/$dev
		#dd if=/dev/zero of=fiotest bs=1M count=1024
		#sync

		fio - <<-ZZ
		[global]
		randrepeat=1
		ioengine=libaio
		iodepth=2048
		direct=1

		blocksize=4k
		#blocksize_range=4k-256k
		loops=100000
		#numjobs=2

		#verify=meta

		[randwrite]
		filename=/dev/$dev
		rw=randwrite
		ZZ
	done

	for dev in $@; do
		#cd /mnt/$dev
		#dd if=/dev/zero of=fiotest bs=1M count=1024
		#sync

		fio - <<-ZZ &
		[global]
		randrepeat=1
		ioengine=libaio
		iodepth=1280
		direct=1

		blocksize=4k
		#blocksize_range=4k-256k
		#size=900M
		loops=100000
		numjobs=1

		#verify=meta

		[randwrite]
		filename=/dev/$dev
		rw=randread
		ZZ
	done
}

test_sync()
{
	while true; do
		sync
		sleep 0.5
	done
}

test_drop_caches()
{
	while true; do
		echo 3 > /proc/sys/vm/drop_caches
		sleep 1
	done
}

# Other stuff

test_stress()
{
	test_sync &
	#test_drop_caches &
	test_dbench &
	test_bonnie &
	#test_fio &
}

test_powerfail()
{
	sleep 120
	echo b > /proc/sysrq-trigger
}

test_mkfs_stress()
{
	prep_mkfs		|| exit
	prep_mounts		|| exit

	test_stress &
	#test_powerfail
}

dmesg -n 7
echo 1 > /proc/sys/kernel/sysrq

ln -sf /sys/kernel/debug/tracing/ /root/t
