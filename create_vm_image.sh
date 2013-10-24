#!/bin/bash
#
# Create a VM image suitable for running automated tests
# Output: vm_image
set -e

. $(dirname $(readlink -f $0))/util.sh

checkdep debootstrap

OUT=vm_image
MNT=vm_image.mnt
SIZE=$((2 * 1024 * 1024 * 1024)) # 2GB

PACKAGES="less,psmisc,openssh-server"
PACKAGES+=",make,gcc,g++,gdb,strace"
PACKAGES+=",xfsprogs,mdadm,lvm2,aoetools,vblade"
PACKAGES+=",linux-tools,blktrace,sysstat,fio,dbench,bonnie++"

EXCLUDE="dmidecode,nano,rsyslog,logrotate,cron,iptables,nfacct"
EXCLUDE+=",debconf-i18n,info"

fallocate -l $SIZE $OUT
mkfs.ext4 -F $OUT
mkdir -p $MNT
mount -o loop $OUT $MNT

debootstrap --include="$PACKAGES" --exclude="$EXCLUDE" sid "$MNT" http://debian-cache:3142/debian/

cat > "$MNT/etc/fstab" <<-ZZ
debugfs				/sys/kernel/debug	debugfs		defaults	0	0
configfgs			/sys/kernel/config	configfs	defaults	0	0
ZZ

cat > "$MNT/etc/network/interfaces" <<-ZZ
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
ZZ

cat > "$MNT/etc/rc.local" <<-ZZ
#!/bin/bash

mount /dev/sr0 /cdrom
PATH=$PATH:/cdrom
cd /cdrom
exec ./rc
ZZ
chmod 755 "$MNT/etc/rc.local"

mkdir -p "$MNT/cdrom"

# This corresponds to the public key in BTools/sms/sshkey
mkdir -p "$MNT/root/.ssh"
cat > "$MNT/root/.ssh/authorized_keys" <<-ZZ
ssh-dss AAAAB3NzaC1kc3MAAACBAOgmIsVSHoBpct0FM04YQLL9udut/V8JkD0d3YCc94jmWGpWrU78r5nzqnmD3ULGlK4VJpfOQHWpeRh3bU6YckWmPQe11CAmhCd949vMGnsetAwQ+8msHtZwzD00EqbIEiA+oOSNL0pMiRJKvIOw04MKghpRf0d/kVCMNBiEZxx7AAAAFQC/WIbfGf4wf0JlVj5ccnPXtayp6wAAAIBMm3svgV2tCRDu2U7z63ognByizPJaoneAlnwI/4yDh/KU0NKrsbI+u2Ctf6ICju84P7GsIP+mveuWur8JabeU+VA5wztMrO/hO9WchatVrW3GDpMyGADNkVVIi8p/pxl3ZMX5FXTzavuQNucm4pUWsuer6v434JeteooyEgsy7wAAAIBPO5TcE8ZFs8R83YEtVc36nBbxH2jSjov0cFOanGutxni/zAO+IjfE5cDCKZMCQ4dbBYNJ3i8uyXiqqk5u2viu1jBTqSMvhbcYrUSiXzZyx+Rrxl1ukIjgoH+PuXdu2L3HSy5G9zCQvPj3NJeohtWW9I82QcLnn9C98h7Axz5d4g==
ZZ
chmod 0600 "$MNT/root/.ssh/authorized_keys"

umount $MNT
rmdir $MNT
