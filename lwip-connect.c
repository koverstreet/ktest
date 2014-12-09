#include <arpa/inet.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <lwipv6.h>

#define die(msg)	perror(msg), exit(EXIT_FAILURE)

static void setnonblocking(int fd)
{
	int opts = fcntl(fd, F_GETFL);
	if (opts < 0)
		die("fcntl(F_GETFL) error");

	if (fcntl(fd, F_SETFL, opts|O_NONBLOCK) < 0)
		die("fcntl(F_SETFL) error");
}

static void lwip_setnonblocking(int fd)
{
	int opts = lwip_fcntl(fd, F_GETFL, 0);
	if (opts < 0)
		die("lwip_fcntl(F_GETFL) error");

	if (lwip_fcntl(fd, F_SETFL, opts|O_NONBLOCK) < 0)
		die("lwip_fcntl(F_SETFL) error");
}

int main(int argc, char **argv)
{
	if (argc < 3)
		die("insufficient arguments");

	char *path = argv[1];

	struct stack *stack = lwip_stack_new();
	if (!stack)
		die("lwip_stack_new() error");

	struct netif *interface = lwip_vdeif_add(stack, path);
	if (!interface)
		die("lwip_vdeif_add() error");

	if (lwip_ifup(interface))
		die("lwip_ifup() error");

	struct ip_addr addr4, mask4;
	IP64_ADDR(&addr4, 10,0,2,100);
	IP64_MASKADDR(&mask4, 255,255,255,0);

	if (lwip_add_addr(interface, &addr4, &mask4))
		die("lwip_add_addr() error");

	int fd = lwip_msocket(stack, AF_INET, SOCK_STREAM, 0);
	if (fd < 0)
		die("lwip_msocket() error");

	struct sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family      = AF_INET;
	addr.sin_addr.s_addr = inet_addr(argv[2]);
	addr.sin_port        = htons(atoi(argv[3]));

	if (lwip_connect(fd, (struct sockaddr *) &addr, sizeof(addr)) < 0)
		die("lwip_connect() error");

	setnonblocking(STDIN_FILENO);
	lwip_setnonblocking(fd);

	while (1) {
		char buf[4096];
		fd_set fds;
		int n;

		FD_ZERO(&fds);
		FD_SET(STDIN_FILENO, &fds);
		FD_SET(fd, &fds);

		lwip_select(fd + 1, &fds, NULL, &fds, NULL);

		while ((n = lwip_read(fd, buf, sizeof(buf))) > 0)
			write(STDOUT_FILENO, buf, n);
		if (!n)
			exit(EXIT_SUCCESS);
		if (n < 0 && errno != EAGAIN)
			die("error reading from socket");

		while ((n = read(STDIN_FILENO, buf, sizeof(buf))) > 0)
			lwip_write(fd, buf, n);
		if (!n)
			exit(EXIT_SUCCESS);
		if (n < 0 && errno != EAGAIN)
			die("error reading from stdin");
	}
}
