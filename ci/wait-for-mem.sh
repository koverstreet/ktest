#!/usr/bin/env bash

set -o nounset
set -o errexit
set -o errtrace

server_has_mem()
{
    readarray -t -d ' ' free_info < <(free|awk '/Mem:/ {print $2 " " $3}')

    local mem_total=${free_info[0]}
    local mem_used=${free_info[1]}

    ((mem_used < mem_total / 2 ))
}

while ! server_has_mem; do
    echo "waiting for server load to go down"
    sleep $(($RANDOM % 20))
done
