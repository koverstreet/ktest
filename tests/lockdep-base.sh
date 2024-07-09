#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier 3

require-kernel-config PROVE_LOCKING
require-kernel-config LOCKDEP_BITS=20
require-kernel-config LOCKDEP_CHAINS_BITS=20

require-kernel-config DEBUG_ATOMIC_SLEEP
require-kernel-config PREEMPT
require-kernel-config DEBUG_PREEMPT

call_base_test lockdep "$@"
