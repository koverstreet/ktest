#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier 3
config-compiler clang

require-kernel-config KMSAN
require-kernel-append panic_on_kmsan=1

call_base_test kmsan "$@"
