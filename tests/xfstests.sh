# xfstests wrapper:

require-lib test-libs.sh

#require-file xfstests

require-kernel-config FAULT_INJECTION,FAULT_INJECTION_DEBUG_FS,FAIL_MAKE_REQUEST
require-kernel-config MD,BLK_DEV_DM,DM_FLAKEY,DM_SNAPSHOT
require-kernel-config BLK_DEV,BLK_DEV_LOOP
require-kernel-config SCSI_DEBUG=m
require-kernel-config USER_NS

# 038,048,312 require > 10G
config-scratch-devs 14G
config-scratch-devs 14G
config-timeout $(stress_timeout)

test_xfstests()
{
    FSTYP="$1"
    TESTS="$2"

    export TEST_DEV=/dev/sdb
    export TEST_DIR=/mnt/test
    export SCRATCH_DEV=/dev/sdc
    export SCRATCH_MNT=/mnt/scratch
    export FSTYP

    (cd $LOGDIR/xfstests; make)

    useradd fsgqa
    ln -sf /bin/bash /bin/sh

#    systemctl mask systemd-udevd.service
#    systemctl stop systemd-udevd.service

    systemctl unmask			\
	lvm2-lvmetad.service		\
	lvm2-monitor.service		\
	lvm2-lvmetad.socket		\
	lvm2-activation.service

    systemctl start			\
	lvm2-lvmetad.service		\
	lvm2-monitor.service		\
	lvm2-lvmetad.socket

    mkdir -p $TEST_DIR $SCRATCH_MNT
    mkfs.$FSTYP $TEST_DEV
    mount $TEST_DEV $TEST_DIR

    cd $LOGDIR/xfstests
    while true; do
	rm -f results/generic/*
	./check $TESTS
    done
}
