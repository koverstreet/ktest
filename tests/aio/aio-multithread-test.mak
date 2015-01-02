aio-multithread-test: aio-multithread-test.c
	cc -static -Wall -o aio-multithread-test aio-multithread-test.c -laio -lpthread
