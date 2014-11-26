#include <arpa/inet.h>
#include <lwipv6.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define die(msg)	perror(msg), exit(EXIT_FAILURE)

int main(int argc, char **argv)
{
	if (argc < 3)
		die("insufficient arguments");

	char *path = argv[1];

	fprintf(stderr, "path is %s\n", path);

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

	printf("connected\n");

	struct sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family      = AF_INET;
	addr.sin_addr.s_addr = inet_addr(argv[2]);
	addr.sin_port        = htons(atoi(argv[3]));

	if (lwip_connect(fd, (struct sockaddr *) &addr, sizeof(addr)) < 0)
		die("lwip_connect() error");

	while (1) {
		char buf[4096];
		fd_set rfds;
		int n;

		FD_ZERO(&rfds);
		FD_SET(STDIN_FILENO, &rfds);
		FD_SET(fd, &rfds);

		lwip_select(fd + 1, &rfds, NULL, NULL, NULL);

		if (FD_ISSET(fd, &rfds)) {
			if ((n = lwip_read(fd, buf, sizeof(buf))) == 0)
				exit(EXIT_SUCCESS);
			write(STDOUT_FILENO, buf, n);
		}

		if (FD_ISSET(STDIN_FILENO,&rfds)) {
			if ((n = read(STDIN_FILENO, buf, sizeof(buf))) == 0)
				exit(EXIT_SUCCESS);
			lwip_write(fd, buf, n);
		}
	}
}
