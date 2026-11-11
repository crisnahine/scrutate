/* A C file. */
#include <stdio.h>

int add(int a, int b);
extern int verbose;

/* Adds two ints. */
int add(int a, int b) { return a + b; }

struct point { int x; int y; };
typedef struct point point_t;
enum color { RED, GREEN };

static int *make_ptr(int v) {
    int *p = malloc(sizeof(int));
    *p = add(v, 1);
    return p;
}
