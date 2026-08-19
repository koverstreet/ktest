#define _GNU_SOURCE

#include <getopt.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define die(msg, ...)					\
do {							\
	fprintf(stderr, msg "\n", ##__VA_ARGS__);	\
	exit(EXIT_FAILURE);				\
} while (0)

static char *mprintf(const char *fmt, ...)
{
	va_list args;
	char *str;
	int ret;

	va_start(args, fmt);
	ret = vasprintf(&str, fmt, args);
	va_end(args);

	if (ret < 0)
		die("insufficient memory");

	return str;
}

static struct timespec xclock_gettime(clockid_t clockid)
{
	struct timespec ts;

	if (clock_gettime(clockid, &ts))
		die("clock_gettime error: %m");
	return ts;
}

static pid_t child;
static int childfd;

static unsigned long	default_timeout;
static unsigned long	timeout;

static char		*logdir;
static char		*test_basename;
static char		*full_log;

static char		*current_test;
static struct timespec	current_test_start;
static FILE		*current_test_log;

static struct timespec	start;
static FILE		*logfile;

static void log_line(const char *fmt, ...)
{
	struct timespec now = xclock_gettime(CLOCK_MONOTONIC);
	va_list args;
	char *msg;

	va_start(args, fmt);
	if (vasprintf(&msg, fmt, args) < 0)
		die("insufficient memory");
	va_end(args);

	char *output = mprintf("%.5lu %s\n", now.tv_sec - start.tv_sec, msg);

	if (current_test_log)
		fputs(output, current_test_log);
	fputs(output, logfile);
	fputs(output, stdout);

	free(output);
	free(msg);
}

static void term_handler(int sig)
{
	fprintf(stderr, "caught signal %i, exiting\n", sig);
	kill(child, SIGTERM);
	exit(EXIT_FAILURE);
}

static void child_handler(int sig)
{
	/* If the child exits early we treat it as a test failure: */
	exit(EXIT_FAILURE);
}

/*
 * Timeout handling escalates: the first alarm injects the FAILED TIMEOUT
 * marker into the guest console — a live guest echoes it back through the
 * output stream, landing it in the logs in order, where the -S/-F exit
 * logic sees it.  But a wedged guest (e.g. a reclaim-deadlocked kernel:
 * console spewing workqueue lockups, nothing reading stdin) echoes
 * nothing, and the supervisor would previously wait on it forever.  So
 * give the echo a grace period, then SIGTERM the child (the preemptively
 * written "TEST FAILED" status stands and SIGCHLD ends the supervisor),
 * then SIGKILL if even that doesn't stick.
 */
static volatile sig_atomic_t timeout_stage;

static void alarm_handler(int sig)
{
	char *msg = mprintf("========= FAILED TIMEOUT %s in %lus\n",
			    current_test ?: "(no test)", timeout);

	switch (timeout_stage++) {
	case 0:
		if (write(childfd, msg, strlen(msg)) == (ssize_t) strlen(msg)) {
			alarm(30);
			break;
		}
		/* Console not even accepting input: skip straight to kill. */
		timeout_stage++;
		/* fallthrough */
	case 1:
		fputs(msg, stderr);
		fputs("guest did not echo timeout marker, killing it\n", stderr);
		kill(child, SIGTERM);
		alarm(10);
		break;
	default:
		fputs("child ignored SIGTERM, sending SIGKILL\n", stderr);
		kill(child, SIGKILL);
		alarm(10);
		break;
	}
	free(msg);
}

static void set_timeout(unsigned long new_timeout)
{
	timeout_stage = 0;
	timeout = new_timeout;
	alarm(new_timeout);
}

static FILE *test_file_open(const char *fname)
{
	char *path = mprintf("%s/%s.%s/%s", logdir, test_basename,
			     current_test, fname);

	FILE *f = fopen(path, "w");
	if (!f)
		die("error opening %s: %m", path);

	free(path);
	setlinebuf(f);
	return f;
}

static FILE *log_open()
{
	char *path = mprintf("%s/%s", logdir, full_log);
	FILE *f = fopen(path, "w");
	if (!f)
		die("error opening %s: %m", path);

	free(path);
	setlinebuf(f);
	return f;
}

/*
 * Strip the line terminator getline() left on, and nothing else.
 *
 * This used to scan for the first iscntrl() character and truncate there,
 * which is a tab: every console line was cut at its first tab, in all three
 * sinks, with no marker. bcachefs formats a lot of diagnostic output with
 * tabstops - that output survived only because printbuf rewrites tabs to
 * spaces once tabstops are pushed. Anything emitting a literal tab lost
 * everything after it, and only in CI, since this is the CI console capture.
 */
static void strim(char *line)
{
	char *p = line + strlen(line);

	while (p > line && (p[-1] == '\n' || p[-1] == '\r'))
		*--p = 0;
}

static const char *str_starts_with(const char *str, const char *prefix)
{
	unsigned len = strlen(prefix);

	if (strncmp(str, prefix, len))
		return NULL;
	return str + len;
}

static char *test_is_starting(const char *line)
{
	const char *testname = str_starts_with(line, "========= TEST   ");
	char *ret, *p;

	if (!testname)
		return NULL;

	ret = strdup(testname);

	while ((p = strchr(ret, '/')))
		*p = '.';

	return ret;
}

static bool test_is_ending(char *line)
{
	return  str_starts_with(line, "========= PASSED ") ||
		str_starts_with(line, "========= FAILED ") ||
		str_starts_with(line, "========= NOTRUN");
}

static FILE *popen_with_pid(char *argv[], pid_t *child)
{
	int pipefd[2];
	if (pipe(pipefd))
		die("error creating pipe: %m");

	*child = fork();
	if (*child < 0)
		die("fork error: %m");

	if (!*child) {
		if (dup2(pipefd[1], STDOUT_FILENO) < 0)
			die("dup2 error: %m");
		if (dup2(pipefd[1], STDERR_FILENO) < 0)
			die("dup2 error: %m");
		close(pipefd[1]);

		int devnull = open("/dev/null", O_RDONLY);
		if (devnull < 0)
			die("error opening /dev/null; %m");
		if (dup2(devnull, STDIN_FILENO) < 0)
			die("dup2 error: %m");
		close(devnull);

		/*
		 * Tell anything downstream that it is already supervised, so
		 * it does not wrap a second supervisor around itself. The CI
		 * runs us around build-test-kernel, which calls start_vm,
		 * which would otherwise start its own around qemu: two
		 * processes parsing the same console, two copies of every
		 * log, two watchdogs, two sets of result directories.
		 */
		if (setenv("KTEST_SUPERVISOR", "1", 1))
			die("setenv error: %m");

		execvp(argv[0], argv);
		die("error execing %s: %m", argv[0]);
	}

	childfd = pipefd[1];
	/* The alarm handler writes to this from signal context; a wedged
	 * guest can leave the console pipe full, and a blocking write would
	 * hang the handler before it can escalate to killing the child. */
	if (fcntl(childfd, F_SETFL, O_NONBLOCK))
		die("fcntl error: %m");

	FILE *childf = fdopen(pipefd[0], "r");
	if (!childf)
		die("fdopen error: %m");

	return childf;
}

static void read_watchdog(const char *line)
{
	const char *new_watchdog = str_starts_with(line, "WATCHDOG ");
	if (new_watchdog)
		set_timeout(atol(new_watchdog));
}

/*
 * QEMU_MONITOR <command>: forward <command> to the VM's monitor socket.
 *
 * Tests need to manipulate the VM from the outside - detach a disk, attach it
 * again - to exercise anything that depends on device timing. The monitor
 * socket is on the host, but a test runs in the guest, so it asks us instead,
 * over the console, the same way set_watchdog already asks for a timeout.
 *
 * That the request travels through the console is the point: the command and
 * whatever the kernel says next land in one log, in order, from one clock.
 *
 * Connected on first use, not at startup: we are exec'd before qemu, so the
 * socket does not exist yet, and a run that never asks should never care
 * whether it appeared. logdir is $ktest_out/out and the socket is its sibling
 * $ktest_out/vm/mon - the same relationship start_vm builds them with.
 *
 * Failure is a warning, not fatal: a test that wanted a device gone will fail
 * its own checks, and that is a better error than the supervisor dying.
 *
 * We write and never read. HMP answers with a banner and prompts we have no
 * use for; a handful of commands per test will not fill the socket buffer.
 */
static int monitor_connect(void)
{
	char *path = mprintf("%s/../vm/mon", logdir);
	struct sockaddr_un addr = { .sun_family = AF_UNIX };
	int fd = -1;

	if (strlen(path) >= sizeof(addr.sun_path)) {
		fprintf(stderr, "monitor socket path too long: %s\n", path);
		goto out;
	}
	strcpy(addr.sun_path, path);

	fd = socket(AF_UNIX, SOCK_STREAM, 0);
	if (fd < 0) {
		fprintf(stderr, "monitor socket: %m\n");
		goto out;
	}

	if (connect(fd, (struct sockaddr *) &addr, sizeof(addr))) {
		fprintf(stderr, "connecting to %s: %m\n", path);
		close(fd);
		fd = -1;
	}
out:
	free(path);
	return fd;
}

/*
 * The monitor is a readline: it echoes every character we send back at us and
 * redraws in place, with cursor-movement escapes. Rendering that the way a
 * terminal would - moving the cursor rather than deleting the escapes - is
 * what makes the echo come back out as exactly the command we sent, so the
 * caller can recognise and drop it. Deleting the escapes instead concatenates
 * successive redraws ("device_addevice_add") and the result matches nothing.
 *
 * Only what a readline actually emits is handled: \r to the left margin, \b
 * and ESC[<n>D back n, ESC[K truncate here, \n end of line. Anything else is
 * dropped, which is right for colour and wrong for nothing qemu sends.
 */
struct render {
	char	line[4096];
	unsigned cursor, len;
};

static void render_putc(struct render *r, char c,
			void (*emit)(void *, const char *), void *ctx)
{
	switch (c) {
	case '\n':
		r->line[r->len] = '\0';
		emit(ctx, r->line);
		r->cursor = r->len = 0;
		break;
	case '\r':
		r->cursor = 0;
		break;
	case '\b':
		if (r->cursor)
			r->cursor--;
		break;
	default:
		if (r->cursor < sizeof(r->line) - 1) {
			r->line[r->cursor++] = c;
			if (r->cursor > r->len)
				r->len = r->cursor;
		}
		break;
	}
}

static void render(struct render *r, const char *buf, size_t n,
		   void (*emit)(void *, const char *), void *ctx)
{
	for (size_t i = 0; i < n; i++) {
		if (buf[i] != '\033') {
			render_putc(r, buf[i], emit, ctx);
			continue;
		}

		if (++i < n && buf[i] == '[') {
			unsigned arg = 0;

			while (++i < n && buf[i] >= '0' && buf[i] <= '9')
				arg = arg * 10 + (buf[i] - '0');

			if (i < n && buf[i] == 'D') {
				unsigned back = arg ?: 1;

				r->cursor = back < r->cursor ? r->cursor - back : 0;
			} else if (i < n && buf[i] == 'K') {
				r->len = r->cursor;
			}
		}
	}
}

/*
 * Everything the monitor has to say goes in the log.
 *
 * qemu answers a command it didn't like - a device id that doesn't exist, a
 * bus that can't hotplug - and says nothing at all when it worked. Without
 * this those two are the same event as far as the guest can tell, and the
 * guest is the only thing watching.
 *
 * What it has to say does NOT include our own command coming back at us, which
 * once rendered arrives as the prompt line and is dropped with the prompt.
 * Logging the raw stream instead cost 48K of log for three commands - one line
 * per keystroke - which is how the replies came to be missed in the first
 * place.
 *
 * Bounded, because the child's console output is blocked behind this: a
 * refusal comes back immediately, and anything slower isn't a refusal.
 */
static void monitor_log_line(void *ctx, const char *l)
{
	const char *cmd = ctx;

	/* the prompt, the connect banner, and our own echo aren't news */
	if (!*l ||
	    str_starts_with(l, "(qemu)") ||
	    str_starts_with(l, "QEMU ") ||
	    str_starts_with(l, "Type 'help'"))
		return;

	if (cmd && str_starts_with(cmd, l))
		return;

	log_line("qemu monitor: %s", l);
}

static void monitor_drain(int fd, const char *cmd)
{
	struct pollfd p = { .fd = fd, .events = POLLIN };
	struct render r = {};
	char buf[4096];

	while (poll(&p, 1, 100) == 1) {
		ssize_t n = read(fd, buf, sizeof(buf));
		if (n <= 0)
			break;

		render(&r, buf, n, monitor_log_line, (void *) cmd);
	}

	/* a reply with no trailing newline still happened */
	if (r.len) {
		r.line[r.len] = '\0';
		monitor_log_line((void *) cmd, r.line);
	}
}

static void monitor_send(const char *cmd)
{
	static int fd = -1;
	static bool failed = false;

	if (failed)
		return;

	if (fd < 0) {
		fd = monitor_connect();
		if (fd < 0) {
			failed = true;
			return;
		}

		monitor_drain(fd, NULL);
	}

	if (dprintf(fd, "%s\n", cmd) < 0)
		fprintf(stderr, "writing to monitor: %m\n");

	monitor_drain(fd, cmd);
}

static void read_monitor_cmd(const char *line)
{
	const char *cmd = str_starts_with(line, "QEMU_MONITOR ");
	if (cmd)
		monitor_send(cmd);
}

static void write_test_file(const char *file, const char *fmt, ...)
{
	va_list args;
	FILE *f = test_file_open(file);

	va_start(args, fmt);
	vfprintf(f, fmt, args);
	va_end(args);

	fclose(f);
}

static void test_start(char *new_test, struct timespec now)
{
	free(current_test);
	current_test		= new_test;
	current_test_start	= now;
	current_test_log	= test_file_open("log");

	write_test_file("status", "TEST FAILED\n");

	set_timeout(default_timeout);
}

static void test_end(struct timespec now)
{
	write_test_file("duration", "%li", now.tv_sec - current_test_start.tv_sec);

	fclose(current_test_log);
	current_test_log = NULL;

	set_timeout(default_timeout);
}

static void usage(void)
{
	puts("supervisor - test supervisor"
	     "Usage: supervisor [OPTIONS] -- <test-command>\n"
	     "\n"
	     "Options\n"
	     "      -S              Exit on success\n"
	     "      -F              Exit on failure\n"
	     "      -T TIMEOUT      Timeout after TIMEOUT seconds\n"
	     "      -b name         base name for log files\n"
	     "      -o dir          output directory for log files\n"
	     "      -h              Display this help and exit");
}

int main(int argc, char *argv[])
{
	bool exit_on_success = false;
	bool exit_on_failure = false;
	int opt, ret = EXIT_FAILURE;

	setlinebuf(stdin);
	setlinebuf(stdout);

	if (clock_gettime(CLOCK_MONOTONIC, &start))
		die("clock_gettime error: %m");

	while ((opt = getopt(argc, argv, "SFT:b:o:f:h")) != -1) {
		switch (opt) {
		case 'S':
			exit_on_success = true;
			break;
		case 'F':
			exit_on_failure = true;
			break;
		case 'T':
			errno = 0;
			default_timeout = strtoul(optarg, NULL, 10);
			if (errno)
				die("error parsing timeout: %m");
			break;
		case 'b':
			test_basename = strdup(optarg);
			break;
		case 'f':
			full_log = strdup(optarg);
			break;
		case 'o':
			logdir = strdup(optarg);
			break;
		case 'h':
			usage();
			exit(EXIT_SUCCESS);
		case '?':
			usage();
			exit(EXIT_FAILURE);
		}
	}

	if (!test_basename)
		die("Required option -b missing");

	if (!logdir)
		die("Required option -o missing");

	struct sigaction term_action = { .sa_handler = term_handler };
	if (sigaction(SIGTERM, &term_action, NULL) ||
	    sigaction(SIGINT, &term_action, NULL) ||
	    sigaction(SIGHUP, &term_action, NULL) ||
	    sigaction(SIGPIPE, &term_action, NULL) ||
	    sigaction(SIGUSR1, &term_action, NULL) ||
	    sigaction(SIGUSR2, &term_action, NULL))
		die("sigaction error: %m");

	FILE *childf = popen_with_pid(argv + optind, &child);

	logfile = log_open();

	size_t n = 0;
	ssize_t len;
	char *line = NULL;

	struct sigaction child_action = { .sa_handler = child_handler };
	if (sigaction(SIGCHLD, &child_action, NULL))
		die("sigaction error: %m");

	struct sigaction alarm_action = { .sa_handler = alarm_handler };
	if (sigaction(SIGALRM, &alarm_action, NULL))
		die("sigaction error: %m");

	set_timeout(default_timeout);
again:
	while ((len = getline(&line, &n, childf)) >= 0) {
		struct timespec now = xclock_gettime(CLOCK_MONOTONIC);

		strim(line);

		read_watchdog(line);

		char *new_test = test_is_starting(line);

		/* If a test is starting, close logfile for previous test: */
		if (current_test_log && new_test)
			test_end(now);

		if (new_test)
			test_start(new_test, now);

		log_line("%s", line);

		/* after logging the line, so a reply follows its command: */
		read_monitor_cmd(line);

		if (current_test_log && test_is_ending(line)) {
			write_test_file("status", "%s\n", line);
			test_end(now);
		}

		if (exit_on_failure && str_starts_with(line, "TEST FAILED"))
			break;

		if (exit_on_failure && strstr(line, "FAILED TIMEOUT"))
			break;

		if (exit_on_success && str_starts_with(line, "TEST SUCCESS")) {
			ret = 0;
			break;
		}

		/*
		 * BUG: / kernel BUG at indicates a real fault; arm a short
		 * post-mortem alarm so the supervisor doesn't sit on a wedged
		 * kernel. Match the colon explicitly so this doesn't fire on
		 * benign substrings like "DEBUG BUILD" / "DEBUG_INFO".
		 */
		if (exit_on_failure &&
		    (strstr(line, "Kernel panic") ||
		     strstr(line, "BUG:") ||
		     strstr(line, "kernel BUG at")))
			alarm(5);
	}

	if (len == -1 && errno == EINTR) {
		clearerr(childf);
		goto again;
	}

	kill(child, SIGTERM);
	exit(ret);
}
