#!/usr/bin/env bash
#
# mmap write amplification: does dirtying one 4k page write back a whole folio?
#
# ->set_page_dirty(page) named a page. ->dirty_folio(mapping, folio) does not,
# so a filesystem asked to dirty a folio has to dirty all of it. With large
# folios that costs folio_size/PAGE_SIZE. iomap_dirty_folio() does exactly this
# (fs/iomap/buffered-io.c), as does block_dirty_folio(), as does bcachefs - none
# of them can do better from that callback.
#
# Reported against bcachefs as 100GB written for a 1GB LMDB database. The body
# is shared because the whole point is that the number should come out the SAME
# on every filesystem - it's set by mm, not by the fs. XFS mattered first: if it
# reproduces on iomap then this is a generic mm problem and not a bcachefs one,
# and iomap is where it also defeats machinery added deliberately - a per-block
# dirty bitmap, consulted at writeback via ifs_find_dirty_range, whose every bit
# the mmap path then sets.
#
# Per-filesystem wrappers set MWA_KCONFIG / MWA_MKFS / MWA_FSTYP.
#
# The measurement is a stride sweep rather than a single ratio. Each pass dirties
# the same number of 4k pages, spread further apart each time, so amplification
# should be min(stride, folio_size)/4096: it tracks the stride while several
# dirtied pages still share a folio, then flattens once each dirtied page has a
# folio to itself. Where it flattens is the folio size, so the output shows the
# folio-vs-page claim directly instead of asserting it. A kernel that tracked
# dirty per page would be flat at ~1x throughout.
#
# Measured on 7.2.0-rc4, XFS, THP on (so MAX_PAGECACHE_ORDER is 9 = 2M):
#
#   stride     4k  dirtied 2048k  written    2065k    1.0x
#   stride    16k  dirtied 2048k  written    2065k    1.0x
#   stride    64k  dirtied 2048k  written   18961k    9.3x
#   stride   256k  dirtied 2048k  written  109201k   53.3x
#   stride  1024k  dirtied 2048k  written  494257k  241.3x
#   stride  2048k  dirtied 2048k  written 1013089k  494.7x
#
# 2MB dirtied, 1,013,089k written. The kernel-side counters in the iomap
# instrumentation patch independently reported 498.74x over the same run, from
# a completely different measurement (bytes handed to ->dirty_folio vs one page
# per fault) than these block-layer sector counts.
#
# That first sweep stopped at 2048k, which IS the max folio size, so it never
# reached the flat part - amplification was still tracking stride at the last
# point. 4096k and 8192k were added to get past the knee: those should come out
# at roughly the same ~500x as 2048k, and that plateau is MAX_PAGECACHE_ORDER
# read straight off the data.

. $(dirname $(readlink -e "${BASH_SOURCE[0]}"))/../test-libs.sh

require-kernel-config ${MWA_KCONFIG:-XFS_FS}
# Without THP the pagecache caps at order 8 rather than 9, halving the effect -
# still visible, but the sweep is less clear.
require-kernel-config TRANSPARENT_HUGEPAGE

config-scratch-devs 12G

# Same number of dirtied pages every pass, so "written" is directly comparable
# across strides. The file has to cover NR_PAGES * the largest stride.
NR_PAGES=512
MAX_STRIDE_K=8192
FILE_MB=$((NR_PAGES * MAX_STRIDE_K / 1024))

sectors_written()
{
    awk '{print $7}' "/sys/block/$1/stat"
}

measure_stride()
{
    local dev=$1 stride_k=$2
    local stride_b=$((stride_k * 1024))
    local region=$((NR_PAGES * stride_b))

    # Fresh pagecache each pass: folio order is decided as the file is faulted
    # in, so folios left over from the previous pass would hide the effect.
    sync
    echo 3 > /proc/sys/vm/drop_caches

    local cmds=("-c" "mmap -rw 0 $region")
    local off=0 i=0
    while [[ $i -lt $NR_PAGES ]]; do
	cmds+=("-c" "mwrite -S 0x41 $off 8")
	off=$((off + stride_b))
	i=$((i + 1))
    done
    cmds+=("-c" "msync -s 0 $region")

    local before=$(sectors_written $dev)
    xfs_io "${cmds[@]}" /mnt/testfile >/dev/null
    sync
    local after=$(sectors_written $dev)

    local written_kb=$(( (after - before) / 2 ))
    local dirtied_kb=$(( NR_PAGES * 4 ))

    printf '  stride %5sk  region %5sM  dirtied %5dk  written %7dk  amplification %s\n' \
	"$stride_k" "$((region / 1024 / 1024))" "$dirtied_kb" "$written_kb" \
	"$(awk -v w=$written_kb -v d=$dirtied_kb \
	     'BEGIN { if (d) printf "%.1fx", w/d; else print "n/a" }')"
}

test_mmap_write_amp()
{
    set_watchdog 900

    local dev=${ktest_scratch_dev[0]}
    local devname=$(basename $(readlink -f $dev))

    if [[ ! -e /sys/block/$devname/stat ]]; then
	echo "FAILED: no /sys/block/$devname/stat - the scratch dev isn't a whole"
	echo "        block device, and amplification is measured from its sectors written"
	exit 1
    fi

    run_quiet "" ${MWA_MKFS:-mkfs.xfs -f} $dev
    mount -t ${MWA_FSTYP:-xfs} $dev /mnt

    # Fully allocated, so the sweep measures overwrite rather than allocation.
    dd if=/dev/urandom of=/mnt/testfile bs=1M count=$FILE_MB oflag=direct
    sync

    echo
    echo "Dirtying $NR_PAGES 4k pages ($((NR_PAGES * 4))k) per pass, spread at increasing stride."
    echo "Amplification should track the stride and then flatten; it flattens at the"
    echo "folio size. A kernel tracking dirty per page would stay near 1x throughout."
    echo

    for stride_k in 4 16 64 256 1024 2048 4096 8192; do
	measure_stride $devname $stride_k
    done

    echo
    echo "iomap instrumentation (TEST PATCH; empty if the kernel is unpatched):"
    dmesg | grep "iomap-wa:" | tail -25 || true

    umount /mnt
}


