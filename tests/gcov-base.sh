#!/usr/bin/env bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

require-gcov fs/bcachefs

call_base_test gcov "$@"
