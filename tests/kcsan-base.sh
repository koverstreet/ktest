#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier 3
config-compiler clang

# arm64 gates HAVE_ARCH_KCSAN behind EXPERT (`select HAVE_ARCH_KCSAN if EXPERT`).
# Pull EXPERT in explicitly so the variant is selectable across arches.
require-kernel-config EXPERT
require-kernel-config KCSAN

call_base_test kcsan "$@"
