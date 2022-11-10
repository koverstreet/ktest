# xfstests wrapper:

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/test-libs.sh

require-git https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git xfstests

require-kernel-config FAULT_INJECTION,FAULT_INJECTION_DEBUG_FS,FAIL_MAKE_REQUEST
require-kernel-config MD,BLK_DEV_DM,DM_FLAKEY,DM_SNAPSHOT,DM_LOG_WRITES
require-kernel-config DM_THIN_PROVISIONING
require-kernel-config BLK_DEV,BLK_DEV_LOOP
require-kernel-config SCSI_DEBUG=m
require-kernel-config USER_NS

# 038,048,312 require > 10G
config-scratch-devs 14G
config-scratch-devs 14G
config-scratch-devs 14G

# swap
config-scratch-devs 2G

config-timeout 7200

list_tests()
{
    (cd $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/xfstests/tests; echo generic/???)
}

run_xfstests()
{
    export FSTYP="$1"
    shift

    if [[ ! -f /xfstests-init-done ]]; then
	mkswap /dev/sde
	swapon /dev/sde

	useradd -m fsgqa
	useradd fsgqa2
	useradd 123456-fsgqa

	mkdir -p /mnt/test /mnt/scratch

	run_quiet "building $(basename $i)" make -j $ktest_cpus -C "$ktest_dir/tests/xfstests"

	rm -rf /ktest-out/xfstests

	wipefs -af /dev/sdb
	mkfs.$FSTYP $MKFS_OPTIONS -q /dev/sdb

	touch /xfstests-init-done
    fi

    # mkfs.xfs 5.19 requires these variables to be exported into its
    # environment to allow sub-300MB filesystems for fstests.
    export TEST_DEV=/dev/sdb
    export TEST_DIR=/mnt/test
    cat << EOF > /ktest/tests/xfstests/local.config
TEST_DEV=$TEST_DEV
TEST_DIR=$TEST_DIR
SCRATCH_DEV=/dev/sdc
SCRATCH_MNT=/mnt/scratch
LOGWRITES_DEV=/dev/sdd
RESULT_BASE=/ktest-out/xfstests
LOGGER_PROG=true
EOF

    export MKFS_OPTIONS
    mount -t $FSTYP /dev/sdb /mnt/test

    cd "$ktest_dir/tests/xfstests"
    ./check "$@"
}
