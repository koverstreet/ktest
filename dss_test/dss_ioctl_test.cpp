/*
 * dss_ioctl_test.cpp
 *
 *  Created on: Dec 3, 2014
 *      Author: oleg
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <uuid/uuid.h>
#include <linux/fs.h>
#include <UnitTest++.h>
#include <TestReporterStdout.h>
#include "span.h"

const char *progname;
const char *blockdev;

#define TEST_SPAN_LEN   4096

void usage(int code)
{
    fprintf(stderr, "usage: %s blockdev\n", progname);
    exit(code);
}

TEST(SimpleOpenOnEmpty)
{
    errno = 0;

    int fd = ::open(blockdev, O_RDWR);
    int syserr = errno;
    CHECK(fd >= 0);
    CHECK_EQUAL(0, syserr);
    if (fd < 0) {
        return;
    }

    do {
        // try getting a span. pass invalid spaninfo
        int ret = ::ioctl(fd, DSS_IOCTL_GET_SPAN, nullptr);
        syserr = errno;
        CHECK_EQUAL(-1, ret);
        CHECK_EQUAL(EINVAL, syserr);

        if (ret != -1) {
            break;
        }

        // try getting a span. there shouldn't be any, so don't bother
        // allocating server lists
        span_t span_0;
        bzero(&span_0, sizeof(span_t));
        span_0.sp_offset = 0;
        span_0.sp_max_sz = TEST_SPAN_LEN;

        errno = 0;
        ret = ::ioctl(fd, DSS_IOCTL_GET_SPAN, &span_0);
        syserr = errno;
        CHECK_EQUAL(-1, ret);
        CHECK_EQUAL(ENOENT, syserr);

    } while (0);
    close(fd);
}

void fake_uuid(uuid_le &uu, const char *prefix, uint i)
{
    // uuid is 16 bytes
    snprintf((char *)&uu.b, 16, "%.8s%-8.8u", prefix, i);
}

TEST(SetSpan)
{
    int ret;
    int syserr;

    errno = 0;

    int fd = ::open(blockdev, O_RDWR);
    syserr = errno;
    CHECK(fd >= 0);
    CHECK_EQUAL(0, syserr);

    if (fd < 0) {
        return;
    }

    do {
        // start by querying how much room there is
        uint64_t lun_size;
        ret = ::ioctl(fd, BLKGETSIZE64, &lun_size);
        syserr = errno;
        CHECK_EQUAL(0, ret);
        CHECK_EQUAL(0, syserr);

        if (ret != 0) {
            break;
        }

        // create a span
        span_t span_0;
        bzero(&span_0, sizeof(span_t));

        span_0.sp_id = 1234;
        span_0.sp_max_sz = TEST_SPAN_LEN;
        span_0.sp_state = SP_ACTIVE;
        span_0.sp_len = TEST_SPAN_LEN;
        span_0.sp_active_map.pm_id = 1;
        //
        pm_layout_t *l = &span_0.sp_active_map.pm_layout_desc;
        strncpy(l->pml_name, "test R1", MAX_PML_NAME - 1) ;
        snprintf(l->pml_spec.pmla_description, MAX_LAYOUT_DESC,
                 "<layout <type \"RAID1\" /> <nmir 3/> />");

        l->pml_servers = 5;
        l->pml_max_failures = 2;
        l->pml_type = PM_LAYOUT_RAID1;

        // fill in the server list
        span_0.sp_active_map.pm_servers = (pm_server_t *)
                    (malloc(sizeof(pm_server_t) * l->pml_servers));

        for (unsigned i = 0; i < l->pml_servers; i++) {
            fake_uuid(span_0.sp_active_map.pm_servers[i].pms_uuid, "srv", i);
            span_0.sp_active_map.pm_servers[i].pms_state = PS_ACTIVE;
            span_0.sp_active_map.pm_servers[i].pms_index = i;
            span_0.sp_active_map.pm_servers[i].pms_eff_index = i;
        }
        span_0.sp_future_map.pm_id = 0;
        span_0.sp_future_map.pm_servers = nullptr;

        ret = ::ioctl(fd, DSS_IOCTL_SET_SPAN, &span_0);
        syserr = errno;
        CHECK_EQUAL(0, ret);
        CHECK_EQUAL(0, syserr);

        if (ret != 0) {
            break;
        }

        // try creating the same span.  should get EEXIST
        ret = ::ioctl(fd, DSS_IOCTL_SET_SPAN, &span_0);
        syserr = errno;
        CHECK_EQUAL(-1, ret);
        CHECK_EQUAL(EEXIST, syserr);

        if (ret != -1) {
            break;
        }

        uint64_t max_span_count = lun_size / TEST_SPAN_LEN;
        // do i have enough space to make 1000 spans?
        CHECK(max_span_count >= 10000);
        // so make 1000 of them, with the 1st one already made...
        for (uint64_t i = 1; i < 10000; i++) {
            // reset errno from last error
            errno = 0;
            // reuse span_0.   need to change id and offset
            span_0.sp_id += 1;
            span_0.sp_offset += TEST_SPAN_LEN;
            ret = ::ioctl(fd, DSS_IOCTL_SET_SPAN, &span_0);
            syserr = errno;
            CHECK_EQUAL(0, ret);
            CHECK_EQUAL(0, syserr);
            if (ret != 0) {
                break;
            }
        }

    } while (0);
    close(fd);
}

int main(int argc, char *argv[])
{
    progname = argv[0];

    // expect device name
    if (argc != 2) {
        usage(1);
    }

    blockdev = argv[1];

    return UnitTest::RunAllTests();
}
