#!/bin/bash

. $(dirname $(readlink -e ${BASH_SOURCE[0]}))/test-libs.sh

# A kernel without Rust: what the bcachefs DKMS module's vendored Rust stack
# builds against when the kernel's own Rust can't be used - CachyOS's ThinLTO
# kernels, and any kernel configured with no rustc (bcachefs-tools #1128).
# kconfig.sh asks for RUST on most arches; take that back.
unrequire-kernel-config RUST
require-kernel-config RUST=n

call_base_test norust "$@"
