#!/usr/bin/python
# ======================================================
# Copyright (c) 2014 Datera, Inc.  All Rights Reserved.
# Datera, Inc. Confidential and Proprietary Information.
# ======================================================

vmbr_ifdn = """#!/bin/bash
set -x

err="ok"
if [ ! $(which brctl) ]; then
  err="brctl"
elif [ ! $(which ip) ]; then
  err="ip"
fi

if [ "$err" != "ok" ]; then
  echo "$0: Failed to remove tap interface - $err command not found"
  exit 1
fi

if [ -z "$1" ]; then
  echo "Error: no interface specified"
  exit 1
fi

brctl delif ${bridge} $1
sleep 0.5s
ip link set $1 down
ip tuntap del dev $1 mode tap
exit 0

"""

vmbr_ifup = """#!/bin/bash
set -x

err="ok"
if [ ! $(which brctl) ]; then
  err="brctl"
elif [ ! $(which ip) ]; then
  err="ip"
fi

if [ "$err" != "ok" ]; then
  echo "$0: Failed to add tap interface - $err command not found"
  exit 1
fi

if [ -z "$1" ]; then
  echo "Error: no interface specified"
  exit 1
fi

ip tuntap add dev $1 mode tap
ip link set $1 up
sleep 0.5s
brctl addif ${bridge} $1
exit 0

"""

