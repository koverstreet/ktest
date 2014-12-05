/*
 * dss_ioctl_test.cpp
 *
 *  Created on: Dec 3, 2014
 *      Author: oleg
 */
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <uuid/uuid.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "span.h"

const char *progname;

void usage(int code)
{
	fprintf(stderr, "usage: %s blockdev\n", progname);
	exit(code);
}
int main(int argc, char *argv[])
{
	progname = argv[0];

	// expect device name
	if (argc != 2) {
		usage(1);
	}

	const char *blockdev = argv[1];
	int fd = ::open(blockdev, O_ACCMODE | O_RDWR | O_DIRECT | O_NONBLOCK);
	if (fd < 0) {
		fprintf(stderr, "Error opening \"%s\": %s\n",
		        blockdev, strerror(errno));
	}
	// try getting a span, there shouldn't be any
	struct span_info span_info_0;
	int ret = ::ioctl(fd, DSS_IOCTL_GET_SPAN, &span_info_0);
	if (ret != 0) {
		fprintf(stderr, "ioctl failed, err=%d\n", ret);
	} else {
		fprintf(stderr, "ioctl succeeded, but this is an error!\n");
	}

	close(fd);
	exit(0);
}
