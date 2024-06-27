#!/bin/bash
# xfstests wrapper:

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/test-libs.sh

require-git https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git xfstests

# disable io_uring - io_uring is currently broken w.r.t. unmounting, we get
# spurious umount failures with -EBUSY
export ac_cv_header_liburing_h=no

require-kernel-config BSD_PROCESS_ACCT
require-kernel-config FAULT_INJECTION,FAULT_INJECTION_DEBUG_FS,FAIL_MAKE_REQUEST
require-kernel-config MD,BLK_DEV_DM,DM_FLAKEY,DM_SNAPSHOT,DM_LOG_WRITES
require-kernel-config DM_THIN_PROVISIONING
require-kernel-config DM_ZERO
require-kernel-config BLK_DEV,BLK_DEV_LOOP
require-kernel-config SCSI_DEBUG=m
require-kernel-config USER_NS
require-kernel-config OVERLAY_FS

# 038,048,312 require > 10G
config-scratch-devs 14G
config-scratch-devs 14G
config-scratch-devs 14G

# swap
config-scratch-devs 2G

config-timeout 7200

list_tests()
{
    pushd $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/xfstests > /dev/null

    for i in tests/*; do
	if [[ -d $i ]]; then
	    pushd $i > /dev/null
	    ../../tools/mkgroupfile group.list
	    popd > /dev/null
	fi
    done

    for g in generic shared "$FSTYP"; do
	[[ ! -f tests/$g/group.list ]] && continue
	grep -hE '[0-9][0-9][0-9] .*(auto|dangerous)' tests/$g/group.list|
	    sed -e "s/ .*//" -e "s/^/$g\//"
    done

    popd > /dev/null
}

run_xfstests()
{
    if [[ ! -f /xfstests-init-done ]]; then
	mkswap ${ktest_scratch_dev[3]}
	swapon ${ktest_scratch_dev[3]}

	useradd -m fsgqa
	useradd fsgqa2
	useradd 123456-fsgqa

	mkdir -p /mnt/test /mnt/scratch

	run_quiet "building $(basename $i)" make -j $ktest_cpus -C "$ktest_dir/tests/xfstests"

	rm -rf /ktest-out/xfstests

	if [[ $FSTYP != "nfs" ]]; then
	    wipefs -af ${ktest_scratch_dev[0]}
	    mkfs.$FSTYP $MKFS_OPTIONS -q ${ktest_scratch_dev[0]}
	fi

	touch /xfstests-init-done
    fi

    # mkfs.xfs 5.19 requires these variables to be exported into its
    # environment to allow sub-300MB filesystems for fstests.
    export TEST_DEV=${ktest_scratch_dev[0]}
    export TEST_DIR=/mnt/test

    if [[ ! -e /ktest/tests/xfstests/local.config ]]; then
	cat << EOF > /ktest/tests/xfstests/local.config
TEST_DEV=${ktest_scratch_dev[0]}
TEST_DIR=$TEST_DIR
SCRATCH_DEV=${ktest_scratch_dev[1]}
SCRATCH_MNT=/mnt/scratch
LOGWRITES_DEV=${ktest_scratch_dev[2]}
RESULT_BASE=/ktest-out/xfstests
LOGGER_PROG=true
EOF
    fi

    export MKFS_OPTIONS

    cd "$ktest_dir/tests/xfstests"
    ./check "$@"
}
