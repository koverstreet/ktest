#!/bin/bash

set -o nounset
set -o errexit
set -o errtrace

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

# Clean stale test jobs:
cd $JOBSERVER_OUTPUT_DIR
find -size 0 -cmin +180 |xargs rm -f > /dev/null

cd $JOBSERVER_HOME/linux
flock --nonblock .git_fetch.lock git fetch --all > /dev/null

~/ktest/lib/get-test-job -b ~/BRANCHES-TO-TEST -o ~/web/c
