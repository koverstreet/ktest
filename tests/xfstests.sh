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
    useradd -m fsgqa || true
    useradd -g fsgqa 123456-fsgqa || true

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
    export FSTYP="$1"
    shift

    cat << EOF > /ktest/tests/xfstests/local.config
TEST_DEV=/dev/sdb
TEST_DIR=/mnt/test
SCRATCH_DEV=/dev/sdc
SCRATCH_MNT=/mnt/scratch
LOGWRITES_DEV=/dev/sdd
RESULT_BASE=/ktest-out/xfstests-results
LOGGER_PROG=true
EOF

    wipefs -af /dev/sdb
    mkfs.$FSTYP -q /dev/sdb

    mount /dev/sdb /mnt/test

    cd "$ktest_dir/tests/xfstests"

    ./check "$@"
}
