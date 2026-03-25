CC = gcc
CFLAGS = -Wall -Wextra -O2 $(shell pkg-config --cflags libpq 2>/dev/null || echo -I/usr/include/postgresql)
LDFLAGS = $(shell pkg-config --libs libpq 2>/dev/null || echo -lpq)

test_driver: test_driver.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

clean:
	rm -f test_driver

.PHONY: clean
