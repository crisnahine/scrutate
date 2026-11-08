# Make sample
CC = gcc
CFLAGS = -O2

# Builds everything.
all: main.o util.o
	$(CC) -o app main.o util.o

clean:
	rm -f *.o app

define banner
	echo hi
endef
