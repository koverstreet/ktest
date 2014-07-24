#!/bin/bash -e

checkdep fallocate util-linux
checkdep mkfs.ext4 e2fsprogs

SIZE="2G"   # See man fallocate and resize2fs for meaning

usage()
{
    echo "create_vm_image: Create a virtual machine image for ktest"
    echo "Usage: create_vm_image [ -m debian mirror ] filename"
}

create_mount_image()
{
    while getopts "hm:" arg; do
        case $arg in
            h)
                echo "-m	debian mirror" 
                ;;
            m)
                MIRROR=$OPTARG
                ;;
        esac
    done
    shift $(( OPTIND - 1 ))

    OUT=$1
    if [ -z "$OUT" ]; then
        usage
        exit
    fi

    if [ `id -u` != 0 ] ; then
        echo this script must be run as root
        exit 1
    fi

    # Use the /tmp tmpfs for the build, its way faster
    TDIR=$(mktemp -d)
    trap 'echo "WARNING: left a mess in: $TDIR"' ERR
    mount -n -t tmpfs none "$TDIR"

    MNT="$TDIR/mount"
    FSFILE="$TDIR/fs"

    fallocate -l $SIZE $FSFILE
    mkfs.ext4 -F $FSFILE
    mkdir -p $MNT
    mount -n -o loop $FSFILE $MNT
}

finish_umount_image()
{
    cp $VM_IMAGE_DIR/fstab "$MNT/etc/fstab"

    touch $MNT/etc/resolv.conf
    chmod 644 $MNT/etc/resolv.conf

    # Blank the root password
    sed -i -e 's/root:[^:]*:/root::/' "$MNT"/etc/shadow

    mkdir -p "$MNT/root/"
    rsync -a -H -S "$MNT/etc/skel/" "$MNT/root/"

    mkdir -p "$MNT/root/.ssh"
    install -m0600 $KTESTDIR/id_dsa.pub "$MNT/root/.ssh/authorized_keys"

    mkdir -p "$MNT/cdrom"
    ln -s /cdrom/modules "$MNT/lib/modules"

    mkdir -p "$MNT/etc/datera" "$MNT/var/datera" "$MNT/var/log/datera" "$MNT/var/log/core"
    chmod 777 "$MNT/var/log/datera" "$MNT/var/log/core"

    # Unmount everything in the root
    i=0
    while [ $i -lt 10 ]; do
        awk '{print $2}' /proc/mounts | grep ^"$MNT"/ | sort -r | {
            while read i_MNT; do
                umount -n "$i_MNT" &>/dev/null || :
            done
        }
        i=$(($i + 1))
    done

    umount $MNT
    rmdir $MNT

    # Trim deleted data from the image (around 75MB)
    e2fsck -f $FSFILE
    resize2fs -M $FSFILE      # shrinks the file
    mv -f $FSFILE $OUT
    resize2fs $OUT $SIZE      # re-grows as sparse

    umount -n $TDIR || :
    rmdir $TDIR && trap ERR   # No longer need the cleanup msg
}
