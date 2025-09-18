#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/../../test-libs.sh

export BCACHEFS_DKMS=1

call_base_test dkms "$@"
