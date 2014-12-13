/*
 * dss_span.cpp
 */
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <uuid/uuid.h>
#include <linux/fs.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <getopt.h>
#include <iostream>

#include "basic_indent_stream.h"
#include "span.h"

const char *progname;
const char *blockdev;

std::ostream& operator<< (std::ostream& os, const pm_layout_spec_t& sp)
{
    os	<< sp.pmla_description;
    return os;
}

std::ostream& operator<< (std::ostream& os, const pm_layout_t& l)
{
    os	<< "name=\"" << l.pml_name << "\"" << std::endl
            << "spec=\"" << l.pml_spec << "\"" << std::endl
            << "type=" << l.pml_type << std::endl
            << "server_count=" << uint32_t(l.pml_servers) << std::endl
            << "max_fail=" << uint32_t(l.pml_max_failures);
    return os;
}

std::ostream& operator<< (std::ostream& os, const pm_server_t& s)
{
    char uu[64];
    uuid_unparse(s.pms_uuid.b, uu);
    os
        << "uuid=" << uu
        << " state=" << s.pms_state
        << " idx=" << uint32_t(s.pms_index)
        << " eff_idx=" << uint32_t(s.pms_eff_index);
    return os;
}

std::ostream& operator<< (std::ostream& os, const placement_map& map)
{
    if (map.pm_id == 0) {
        // this is an empty/invalid map
        os << "(null map)";
        return os;
    }

    os 	<< "id=" << uint32_t(map.pm_id) << std::endl
            << "layout:" << std::endl
            << rts::indent
            << map.pm_layout_desc
            << rts::unindent
            << std::endl
            << "servers:" << std::endl;
    os << rts::indent;
    if (map.pm_servers != nullptr) {
        for (uint i = 0; i < map.pm_layout_desc.pml_servers; i++)
        {
            os << "server=(" << map.pm_servers[i] << ")" << std::endl;
        }
    } else {
        os << "(no list)";
    }
    os << rts::unindent;
    return os;
}

std::ostream& operator<< (std::ostream& os, const span_t& span)
{
    os
        << "id=" << span.sp_id << std::endl
        << "state=" << span.sp_state << std::endl
        << "offset=" << span.sp_offset << std::endl
        << "len=" << span.sp_len << std::endl
        << "max=" << span.sp_max_sz << std::endl
        << "active: " << std::endl
        << rts::indent
        << span.sp_active_map << std::endl
        << rts::unindent
        << "future: " << std::endl
        << rts::indent
        << span.sp_future_map
        << rts::unindent;

    return os;
}



void usage(int code)
{
    fprintf(stderr, "usage: %s op [options] blockdev\n", progname);
    exit(code);
}

struct cmd_opt {
    uint64_t    start_off_;
    uint64_t    end_off_;
    bool        all_;
    const char *	dev_;
};

#define SRV_CNT		10

int dump(const cmd_opt& opt)
{
    int syserr;

    int fd = ::open(opt.dev_, O_RDONLY);
    if (fd < 0) {
        syserr = errno;
        perror("open");
        return syserr;
    }

    span_t span_0;
    uint64_t start_off = opt.start_off_;
    uint64_t end_off = opt.end_off_;
    bool all = opt.all_;
    // poison the data to spot bad transfers
    memset(&span_0, 0xff, sizeof(span_t));
    span_0.sp_offset = opt.start_off_;

    // get the size of the device
    uint64_t lun_size;
    int ret = ::ioctl(fd, BLKGETSIZE64, &lun_size);
    if (ret < 0) {
        syserr = errno;
        std::cout << "failed to get device size";
        ::close(fd);
        return syserr;
    }

    if (lun_size == 0) {
        // zero sized lun?  nothing to do here
        return 0;
    }

    if (all) {
        // going to adjust it later, when i have the lenght of a span,
        // to avoid starting at the very end of the lun.
        end_off = lun_size;
    }
    // make sure the kernel has place to put server lists,
    // overprovision with abandon
    span_0.sp_active_map.pm_layout_desc.pml_servers = SRV_CNT;
    pm_server_t *s_active = (pm_server_t *) malloc(sizeof(pm_server_t) * SRV_CNT);
    span_0.sp_active_map.pm_servers = s_active;

    span_0.sp_future_map.pm_layout_desc.pml_servers = SRV_CNT;
    pm_server_t *s_future = (pm_server_t *) malloc(sizeof(pm_server_t) * SRV_CNT);
    span_0.sp_future_map.pm_servers = s_future;

    do {
        span_0.sp_offset = start_off;
        ret = ::ioctl(fd, DSS_IOCTL_GET_SPAN, &span_0);
        if (ret < 0) {
                if (ret == -ENOSPC) {
                    std::cout << "insufficient space for server lists";
                } else {
                    syserr = errno;
                    perror("ioctl GET");
                    ::close(fd);
                    return syserr;
                }
        }

        // dump the span to stdout
        rts::indent_ostream output(std::cout);
        output << "span @ offset "<< start_off << std::endl;
        output << rts::indent << span_0 << rts::unindent << std::endl;

        // adjust the end to not be past the end of the lun
        if (end_off >= lun_size) {
            end_off = ((lun_size - 1) / span_0.sp_max_sz) * span_0.sp_max_sz;
        }
        start_off += span_0.sp_max_sz;
    } while(start_off <= end_off);

    ::close(fd);
    return 0;
}

int del_span(const cmd_opt& opt)
{
    int syserr = 0;
    int fd = ::open(opt.dev_, O_RDWR);
    do {
        if (fd < 0) {
            syserr = errno;
            perror("open");
            break;
        }

        // set up a span skeleton
        span_t span_0;
        span_0.sp_offset = opt.start_off_;
        int ret = ::ioctl(fd, DSS_IOCTL_DEL_SPAN, &span_0);
        if (ret < 0) {
            syserr = errno;
            perror("ioctl DEL");
        }
        ::close(fd);
    } while (0);
    return syserr;
}

struct option long_options[] =
{
 {"offset", required_argument, 0, 'o'},
 {"length", required_argument, 0, 'l'},
 {"maxlen", required_argument, 0, 'm'},
 {"state", required_argument, 0, 's'},
 {"all", no_argument, 0,  'a'},
 {0, 0, 0, 0}
};

typedef int(*action_fn)(const cmd_opt&);

struct cmd_action {
    const char *cmd;
    action_fn	action;
};

cmd_action actions[] = {
                        { "dump", dump},
                        { "del", del_span}
};

action_fn get_action(const char *cmd)
{
    for (uint i = 0; i < sizeof(actions); i++) {
        if (!strcmp(cmd, actions[i].cmd)) {
            return actions[i].action;
        }
    }
    return nullptr;
}

int main(int argc, char *argv[])
{
    progname = argv[0];

    int opt;
    int c;

    if (argc < 3) {
        usage(1);
    }

    char *cmd = argv[1];

    struct cmd_opt o;
    o.dev_ = argv[2];
    o.all_ = false;

    argv += 2; argc -= 2;

    while ((c = getopt_long(argc, argv, "ao:l:m:s:", long_options, &opt)) != -1)
    {
        switch (c) {
        case 'o':
            // can't tell if it's an error or a 0.  either will do...
            o.end_off_ = o.start_off_ = strtoull(optarg, nullptr, 10);
            break;
        case 'a':
            // operate on all
            o.start_off_ = 0;
            o.all_ = true;
        default:
            // for now, ignore everything else
            break;
        }
    }

    action_fn fn = get_action(cmd);
    if (fn == nullptr) {
        std::cerr << "invalid command " << cmd << std::endl;
        return -1;
    }

    int ret = fn(o);
    return ret;
}
