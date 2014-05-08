#!/usr/bin/python

import sys
import time
import copy
import subprocess

# define the master low-order ip octet - all others are slaves
_MASTER = "2"

# define the ethernet device to use as the ip address source
_DEVICE = "eth0"

class TestServices(object):

    def __init__(self, vm_count):

        # initialize attributes
        self.ip = None
        self.master = None
        self._slaves = []
        self._keyfile = "/cdrom/id_dsa"

        # get ip addr
        lines = subprocess.check_output(["ip","addr","list",_DEVICE])
        ips = [line for line in lines.splitlines() if "inet " in line]
        if len(ips) == 0:
            return
        self.ip = ips[0].split()[1].split("/")[0]

        # identify the master ip address
        ips = self.ip.split(".")
        ips[3] = _MASTER
        self.master = ".".join(ips)

        # gather the list of slave ip addresses
        slaves = vm_count - 1
        for slave in range(slaves):
            ips[3] = str(slave+int(_MASTER)+1)
            self._slaves.append(".".join(ips))

        return

    def is_master(self):
        return self.ip == self.master

    def is_slave(self):
        if self.ip in self._slaves:
            return True
        return not self.is_master()

    def slaves(self):
        # copy the slave list (so caller can't muck with it)
        return copy.copy(self._slaves)

    def peers(self):
        # if caller is the master, just return the slave list
        if self.is_master():
            return self.slaves()
        # otherwise return slave list but remove caller's ip addr
        return [slave for slave in self._slaves if slave != self.ip]

    def _match_expected_results(self,expect,results):
        # initially this is just an equal comparison
        # it can be enhanced later to provide templating
        # and masking for more flexible comparisons
        # returns true if there is a diff else false
        return expect == results

    def ssh(self,rcvr,cmd,expect=None,repeat=1,wait=1):

        # create the ssh command to connect vms
        self._ssh_cmd = ["ssh", "-o", "StrictHostKeyChecking=no",
            "-i", self._keyfile, "root@"+rcvr, "'%s'" % cmd]

        # do the ssh command <repeat> times until success.
        for i in range(repeat):

	    try:
                self.results = subprocess.check_output(self._ssh_cmd,
                    stderr=subprocess.STDOUT)
            except subprocess.CalledProcessError as e:
                print "SSH command '%s' failed" % cmd
                print "Waiting %d seconds..." % wait
                #print e.args
                #print e.__dict__
                #raise
            else:
                print self.results
                if not expect:
                    return True

                if self._match_expected_results(expect,self.results):
                    return True

            time.sleep(wait)

            # ssh command never succeeded - return error
        print "Completed %d iterations... returning with failure" % repeat
        return False
