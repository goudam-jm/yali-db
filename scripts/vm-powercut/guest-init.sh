#!/bin/sh
set -eu
export PATH=/bin:/sbin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp
stty -echo -onlcr
modprobe virtio_mmio || true
modprobe virtio_pci || true
modprobe virtio_blk
modprobe crc32c_generic
modprobe ext4
modprobe xfs
# This RAM-only guest has no host shares, network, or passthrough devices.
# The sole disk is the harness-owned NBD export attached as virtio /dev/vda.
[ -b /dev/vda ]
[ ! -b /dev/vdb ]
echo "BOOT $(uname -r) $(uname -m)"
read -r action filesystem
case "$filesystem" in ext4|xfs) ;; *) exit 10;; esac
case "$action" in
  FORMAT)
    if [ "$filesystem" = ext4 ]; then
      mkfs.ext4 -F -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 /dev/vda
    else
      mkfs.xfs -f -K /dev/vda
    fi
    ;;
  RECOVER) ;;
  *) exit 11;;
esac
if ! mount -t "$filesystem" -o noatime /dev/vda /data; then
    echo MOUNT_FAILED
    while :; do sleep 3600; done
fi
cat /proc/mounts
echo MOUNTED
if [ "$action" = FORMAT ]; then
    sync
    echo READY
    read -r command seed
    [ "$command" = RUN ]
    exec /storage_probe worker /data/db "$seed"
else
    if /storage_probe dump /data/db; then
        echo DUMP_DONE
    else
        echo DUMP_FAILED
    fi
    # Remain mounted and idle; the host controller ends this independent guest.
    while :; do sleep 3600; done
fi
