#!/usr/bin/env bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../test-libs.sh

require-kernel-config UBSAN

call_base_test ubsan "$@"
