# xfstests wrapper:

require-lib test-libs.sh

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

config-timeout 7200

hook_make_xfstests()
{
    useradd -m fsgqa
    useradd -g fsgqa 123456-fsgqa

    mkdir -p /mnt/test /mnt/scratch

    rm -f /ktest/tests/xfstests/results/generic/*

    make -C /ktest/tests/xfstests
}

list_tests()
{
    (cd "/ktest/tests/xfstests/tests"; echo generic/???)
}

run_xfstests()
{
    TEST_DEV=/dev/sdb
    TEST_DIR=/mnt/test
    SCRATCH_DEV=/dev/sdc
    SCRATCH_MNT=/mnt/scratch
    LOGWRITES_DEV=/dev/sdd
    FSTYP="$1"
    shift

    rm /ktest/tests/xfstests/local.config
    echo "TEST_DEV=$TEST_DEV"		>> /ktest/tests/xfstests/local.config
    echo TEST_DIR=$TEST_DIR		>> /ktest/tests/xfstests/local.config
    echo SCRATCH_DEV=$SCRATCH_DEV	>> /ktest/tests/xfstests/local.config
    echo SCRATCH_MNT=$SCRATCH_MNT	>> /ktest/tests/xfstests/local.config
    echo LOGWRITES_DEV=$LOGWRITES_DEV	>> /ktest/tests/xfstests/local.config
    echo FSTYP=$FSTYP			>> /ktest/tests/xfstests/local.config
    echo LOGGER_PROG=true		>> /ktest/tests/xfstests/local.config

    wipefs -af $TEST_DEV
    mkfs.$FSTYP -q $TEST_DEV

    mount $TEST_DEV $TEST_DIR

    cd "$ktest_dir/tests/xfstests"

    ./check "$@"
}
