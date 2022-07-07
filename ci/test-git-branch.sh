#!/bin/bash

JOBSERVER=$1

while true; do
    source <(ssh $JOBSERVER cat .ktestrc bin/_test-git-branch.sh)
done
