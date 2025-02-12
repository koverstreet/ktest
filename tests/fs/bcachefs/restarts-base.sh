#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../../test-libs.sh

require-kernel-config BCACHEFS_INJECT_TRANSACTION_RESTARTS

call_base_test restarts "$@"
