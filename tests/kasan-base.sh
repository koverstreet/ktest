#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier   5

require-kernel-config KASAN
require-kernel-config KASAN_VMALLOC
require-kernel-append kasan.fault=panic

call_base_test kasan "$@"
