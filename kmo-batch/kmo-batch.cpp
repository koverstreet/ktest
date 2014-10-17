#define _FILE_OFFSET_BITS	64
#define __USE_FILE_OFFSET64
#define _XOPEN_SOURCE 600

#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <list>

#define SERVER_SOCKET_PATH	"/var/run/kmo-batch/socket"

#define MAX_JOBS_DEFAULT	4

static unsigned long max_jobs = MAX_JOBS_DEFAULT;

template<typename X, int N>
inline int arraysize(X (&x)[N]) { return N; }

static void make_daemon(void)
{
    auto pid = fork();
    if (pid < 0) {
	perror("fork error");
	exit(EXIT_FAILURE);
    }

    if (pid > 0)
	exit(EXIT_SUCCESS);

    auto sid = setsid();
    if (sid < 0) {
	perror("setsid error");
	exit(EXIT_FAILURE);
    }

    if (chdir("/") < 0) {
	perror("chdir error");
	exit(EXIT_FAILURE);
    }

    auto devnull = open("/dev/null", O_RDWR);
    if (devnull < 0) {
	perror("error opening /dev/null");
	exit(EXIT_FAILURE);
    }

    dup2(devnull, STDIN_FILENO);
    dup2(devnull, STDOUT_FILENO);
    dup2(devnull, STDERR_FILENO);
    close(devnull);
}

static void epoll_watch(int epollfd, int fd, uint32_t events)
{
    struct epoll_event ev;

    memset(&ev, 0, sizeof(ev));
    ev.events = events;
    ev.data.fd = fd;

    if (epoll_ctl(epollfd, EPOLL_CTL_ADD, fd, &ev)) {
	perror("epoll_ctl error");
	exit(EXIT_FAILURE);
    }
}

enum cmd : uint8_t {
    LIST_JOBS,
    NEW_JOB,
    START_JOB,
};

struct job {
    uint64_t mem_size;
};

struct client {
    int		fd;

    time_t	add_time;
    time_t	start_time;

    struct job	job;
};

typedef std::list<struct client> client_list;

static void jobs_list(client_list *pending_jobs,
		      client_list *running_jobs,
		      int fd)
{
    auto f = fdopen(fd, "w");

    fprintf(f, "Pending jobs:\n");

    for (auto &i : *pending_jobs)
	fprintf(f, "added %s\n", ctime(&i.add_time));

    fprintf(f, "Running jobs:\n");

    for (auto &i : *running_jobs)
	fprintf(f, "added %s started %s\n",
		ctime(&i.add_time),
		ctime(&i.start_time));

    fclose(f);
}

static void job_drop(client_list *clients, int fd)
{
    for (auto i = clients->begin(); i != clients->end(); i++)
	if (i->fd == fd) {
	    close(i->fd);
	    clients->erase(i);
	    break;
	}
}

static void job_new(client_list *clients,
		    int epollfd, int fd)
{
    struct client client;
    memset(&client, 0, sizeof(client));

    client.fd = fd;

    auto ret = recv(fd, &client.job, sizeof(client.job), MSG_WAITALL);
    if (ret != sizeof(client.job)) {
	fprintf(stderr, "error reading job description\n");
	close(fd);
	return;
    }

    struct timeval tv;
    gettimeofday(&tv, NULL);

    client.add_time = tv.tv_sec;

    clients->push_back(client);

    epoll_watch(epollfd, fd, EPOLLRDHUP|EPOLLET);
}

static void do_connect(client_list *pending_jobs,
		       client_list *running_jobs,
		       int epollfd, int listenfd)
{
    while (1) {
	auto fd = accept(listenfd, NULL, NULL);

	if (fd < 0) {
	    if (errno != EAGAIN)
		perror("error accepting connection");
	    return;
	}

	cmd cmd;

	auto ret = recv(fd, &cmd, sizeof(cmd), MSG_WAITALL);
	if (ret != sizeof(cmd)) {
	    fprintf(stderr, "error reading command from client\n");
	    close(fd);
	    continue;
	}

	switch (cmd) {
	case LIST_JOBS:
	    jobs_list(pending_jobs, running_jobs, fd);
	    break;
	case NEW_JOB:
	    job_new(pending_jobs, epollfd, fd);
	    break;
	default:
	    fprintf(stderr, "bad command %u\n", cmd);
	    close(fd);
	    break;
	}
    }
}

static bool should_run_job(client_list *running_jobs,
			   const struct client &client)
{
    return running_jobs->size() < max_jobs;
}

static void run_jobs(client_list *pending_jobs,
		     client_list *running_jobs)
{
    while (!pending_jobs->empty() &&
	   should_run_job(running_jobs, pending_jobs->front())) {
	auto client = pending_jobs->front();

	pending_jobs->pop_front();

	cmd cmd = START_JOB;
	if (send(client.fd, &cmd, sizeof(cmd), MSG_WAITALL) == sizeof(cmd))
	    running_jobs->push_back(client);
	else {
	    fprintf(stderr, "error sending START_JOB to client\n");
	    close(client.fd);
	}
    }
}

static int cmd_daemon(int argc, char **argv)
{
    int detach = 0;
    int c;
    struct option opts[] = {
	{ "detach",		0, &detach,	'd' },
	{ "max-jobs",		1, NULL,	'm' },
	{ NULL,			0, NULL,	0 },
    };

    while ((c = getopt_long(argc, argv,
			    "d",
			    opts, NULL)) != -1)
	switch (c) {
	case 'd':
	    detach = 1;
	    break;
	case 'm':
	    errno = 0;
	    max_jobs = strtoul(optarg, NULL, 10);
	    if (errno || !max_jobs) {
		fprintf(stderr, "Bad max jobs %s\n", optarg);
		exit(EXIT_FAILURE);
	    }
	    break;
	}

    if (detach)
	make_daemon();

    auto listenfd = socket(AF_UNIX, SOCK_STREAM|SOCK_NONBLOCK, 0);

    struct sockaddr_un addr;
    addr.sun_family = AF_UNIX;
    strcpy(addr.sun_path, SERVER_SOCKET_PATH);

    unlink(SERVER_SOCKET_PATH);

    if (bind(listenfd, (struct sockaddr *) &addr, sizeof(addr))) {
	perror("bind error");
	exit(EXIT_FAILURE);
    }

    if (listen(listenfd, 4)) {
	perror("listen error");
	exit(EXIT_FAILURE);
    }

    auto epollfd = epoll_create1(EPOLL_CLOEXEC);

    epoll_watch(epollfd, listenfd, EPOLLIN|EPOLLET);

    client_list pending_jobs, running_jobs;

    while (1) {
	struct epoll_event events[32];
        auto nr_events = epoll_wait(epollfd, events, arraysize(events), 2000);

        if (nr_events < 0) {
            if (errno==EINTR)
		continue;

	    perror("epoll_wait error");
	    exit(EXIT_FAILURE);
        }

        for (auto ev = events; ev < events + nr_events; ev++)
	    if (ev->data.fd == listenfd) {
		do_connect(&pending_jobs,
			   &running_jobs,
			   epollfd, listenfd);
	    } else {
		/* a connection was closed - search lists for it */

		job_drop(&pending_jobs, ev->data.fd);
		job_drop(&running_jobs, ev->data.fd);
	    }

	run_jobs(&pending_jobs, &running_jobs);
    }

    return 0;
}

static int cmd_list(int argc, char **argv)
{
    auto daemonfd = socket(AF_UNIX, SOCK_STREAM, 0);

    struct sockaddr_un addr;
    addr.sun_family = AF_UNIX;
    strcpy(addr.sun_path, SERVER_SOCKET_PATH);

    if (connect(daemonfd, (struct sockaddr *) &addr, sizeof(addr))) {
	perror("Error connecting to daemon socket");
	exit(EXIT_FAILURE);
    }

    cmd cmd = LIST_JOBS;

    if (send(daemonfd, &cmd, sizeof(cmd), MSG_WAITALL) != sizeof(cmd)) {
	perror("Error talking to daemon");
	exit(EXIT_FAILURE);
    }

    char buf[1024];
    size_t bytes;

    while ((bytes = read(daemonfd, buf, sizeof(buf))) > 0)
	write(STDOUT_FILENO, buf, bytes);

    return 0;
}

static int cmd_run(int argc, char **argv)
{
    auto daemonfd = socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC, 0);

    struct sockaddr_un addr;
    addr.sun_family = AF_UNIX;
    strcpy(addr.sun_path, SERVER_SOCKET_PATH);

    if (connect(daemonfd, (struct sockaddr *) &addr, sizeof(addr))) {
	perror("Error connecting to daemon socket");
	exit(EXIT_FAILURE);
    }

    cmd cmd = NEW_JOB;
    struct job job = { .mem_size = 0 };

    if (send(daemonfd, &cmd, sizeof(cmd), MSG_WAITALL) != sizeof(cmd) ||
	send(daemonfd, &job, sizeof(job), MSG_WAITALL) != sizeof(job)) {
	perror("Error talking to daemon");
	exit(EXIT_FAILURE);
    }

    if (recv(daemonfd, &cmd, sizeof(cmd), MSG_WAITALL) != sizeof(cmd)) {
	perror("Error talking to daemon");
	exit(EXIT_FAILURE);
    }

    if (cmd != START_JOB) {
	fprintf(stderr, "Bad response from server %u\n", cmd);
	exit(EXIT_FAILURE);
    }

    auto pid = fork();
    if (pid < 0) {
	perror("fork error");
	exit(EXIT_FAILURE);
    } else if (pid) {
	int status = 1;

	while (!(wait(&status) == -1 && errno == ECHILD))
	    ;

	return status;
    } else {
	execvp(argv[1], argv + 1);
	fprintf(stderr, "Error running command\n");
	exit(EXIT_FAILURE);
    }
}

static void usage(void)
{
    printf("kmo-batch: simple dumb batch scheduler\n"
	   "commands:\n"
	   "\tdaemon\tstart scheduler daemon\n"
	   "\tlist\tlist waiting/running jobs\n"
	   "\trun\trun a job\n");
    exit(EXIT_FAILURE);
}

int main(int argc, char **argv)
{
    if (argc < 2) {
	printf("Please supply a command\n");
	usage();
    }

    auto *cmd = argv[1];
    argc -= 1;
    argv += 1;

    if (!strcmp(cmd, "daemon"))
	return cmd_daemon(argc, argv);

    if (!strcmp(cmd, "list"))
	return cmd_list(argc, argv);

    if (!strcmp(cmd, "run"))
	return cmd_run(argc, argv);

    printf("Bad command %s\n", cmd);
    usage();
    return 0;
}
