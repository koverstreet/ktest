#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier 3

require-kernel-config PROVE_LOCKING
require-kernel-config LOCKDEP_BITS=20
require-kernel-config LOCKDEP_CHAINS_BITS=20

require-kernel-config DEBUG_ATOMIC_SLEEP
require-kernel-config PREEMPT
require-kernel-config DEBUG_PREEMPT

# CONFIG_RUST + lockdep needs DMA_SHARED_BUFFER: PROVE_LOCKING pulls in
# DEBUG_MUTEXES, which flips dma_resv_reset_max_fences() (called from the inline
# dma_resv_unlock()) from an empty inline to an extern defined in
# drivers/dma-buf/dma-resv.c — built only under DMA_SHARED_BUFFER. The Rust
# dma_resv helper references it, so without this the vmlinux link fails with an
# undefined reference. Other variants lack DEBUG_MUTEXES and so don't hit it.
# DMA_SHARED_BUFFER has no prompt (select-only), so request it via DMABUF_HEAPS,
# the lightest user-settable symbol that selects it.
require-kernel-config DMABUF_HEAPS

call_base_test lockdep "$@"
