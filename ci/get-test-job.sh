#!/bin/bash

[[ -f ~/.ktestrc ]] && . ~/.ktestrc

# Clean stale test jobs:

cd $JOBSERVER_OUTPUT_DIR

if [[ ! -f stale-job-cleanup ]]; then
    touch stale-job-cleanup
fi

if [[ $(find stale-job-cleanup -mmin +5) ]]; then
    echo -n "Cleaning stale jobs.. " >&2
    find -size 0 -cmin +180 |xargs rm -f > /dev/null
    touch stale-job-cleanup
    echo " done" >&2
fi

cd $JOBSERVER_HOME/linux
flock --nonblock .git_fetch.lock git fetch --all > /dev/null

make -C ~/ktest/lib get-test-job 1>&2

~/ktest/lib/get-test-job -b ~/BRANCHES-TO-TEST -o $JOBSERVER_OUTPUT_DIR
