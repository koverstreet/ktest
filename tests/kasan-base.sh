#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier   2
# KASAN+RUST requires clang (upstream 1b1cac9887ec: `depends on !KASAN ||
# CC_IS_CLANG` on config RUST). With gcc the kernel configures cleanly with
# RUST=n, and require-kernel-config RUST then fails the build.
config-compiler clang

require-kernel-config KASAN
require-kernel-config KASAN_VMALLOC
require-kernel-append kasan.fault=panic

call_base_test kasan "$@"
