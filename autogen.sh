#!/bin/bash

set -e

export ACLOCAL_FLAGS=""
export ACLOCAL_AMFLAGS="-I m4"

aclocal $ACLOCAL_FLAGS

if glibtoolize -h > /dev/null 2>&1 ; then
   glibtoolize --copy --force
else
   libtoolize --copy --force
fi
if [ -f autogen.py ] ; then python autogen.py ; fi

autoheader
automake --copy --add-missing --foreign -Wall -Wno-portability
autoconf
