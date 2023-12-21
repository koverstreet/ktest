#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../test-libs.sh

config-timeout-multiplier 3

require-kernel-config KASAN
require-kernel-config KASAN_VMALLOC

call_base_test kasan "$@"
