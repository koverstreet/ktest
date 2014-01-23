all: cache_torture_test.o
	c99 cache_torture_test.o -o cache_torture_test

cache_torture_test.o: cache_torture_test.c
	c99 -Wall -c cache_torture_test.c

clean:
	rm -rf *o cache_torture_test
