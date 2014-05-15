#!/usr/bin/python
# ======================================================
# Copyright (c) 2014 Datera, Inc.  All Rights Reserved.
# Datera, Inc. Confidential and Proprietary Information.
# ======================================================

from __future__ import print_function
import os
import sys
import string
import subprocess
import random
import os.path
import platform

distro_types = {
    "debian" : {
        "installer":"apt-get",
        "names":["debian","ubuntu"]
    },
    "fedora" : {
        "installer":"yum",
        "names":[ "red hat","rhel","fedora","centos"]
    },
}

def get_installer():
    distro = platform.linux_distribution()[0].lower()
    for distro_type in distro_types.keys():
        for name in distro_type["names"]:
            if name in distro:
                return distro_type["installer"]
    return "INSTALLER_NOT_FOUND"

def die(msg=None, status=1):
    if msg:
        print("Fatal error: %s" % msg, file=sys.stderr)
        sys.exit(status)

def warning(msg=None):
    if msg:
        print("Warning: %s" % msg, file=sys.stderr)

def randstr(size=12, chars=string.letters + string.digits):
    return "".join(random.choice(chars) for x in range(size))

def root():
    uid = int(subprocess.check_output(["id","-u"]))
    return True if uid == 0 else False

def process_template(template_in, template_out, args):
    # if filename is passed, read file, else use string
    if os.path.exists(template_in):
        with open(template_in,"r") as f:
            template = string.Template(f.read())
    else:
        template = string.Template(template_in)

    with open(template_out, "w") as f:
        f.write(template.safe_substitute(args))

def is_executable(command, package=None):

    try:
        subprocess.check_output(["which",command])
        return True
    except:
        if package == "DO_NOT_INSTALL":
            warning("Can not install <%s> command" % command)
            return False

    if not package:
        package = command

    installer = get_installer()

    if not is_executable(installer,"DO_NOT_INSTALL"):
        return False

    try:
        subprocess.check_output([installer,"install",package])
    except:
        warning("Can not install <%s> package" % command)
        return False

    try:
        subprocess.check_output(["which",command])
        return True
    except:
        warning("Can not execute <%s> command" % command)
        return False

