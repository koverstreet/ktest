#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

config-timeout-multiplier 3
config-compiler clang

require-kernel-config KMSAN

call_base_test kmsan "$@"
