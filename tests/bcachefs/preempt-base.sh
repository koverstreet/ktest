#!/usr/bin/env bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../test-libs.sh

require-kernel-config PREEMPT

call_base_test preempt "$@"
