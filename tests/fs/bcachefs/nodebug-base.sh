#!/usr/bin/env bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../../test-libs.sh

require-kernel-config PREEMPT
export NO_BCACHEFS_DEBUG=1

call_base_test nodebug "$@"
