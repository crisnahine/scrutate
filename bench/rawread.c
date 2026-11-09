/* Hardware ceiling for a search: read every byte of every file in a list, in parallel, and do
 * nothing else. A search tool cannot be faster than this on the same files and cache state.
 *
 *   cc -O2 -pthread -o rawread bench/rawread.c
 *   rawread LIST THREADS      (LIST: NUL-separated paths relative to the current directory)
 */
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static char **paths;
static size_t npaths;
static atomic_size_t next_path;
static atomic_ullong total;

static void *worker(void *arg) {
    (void)arg;
    size_t cap = 256 * 1024;
    char *buf = malloc(cap);
    unsigned long long mine = 0;
    for (;;) {
        size_t i = atomic_fetch_add(&next_path, 1);
        if (i >= npaths) break;
        int fd = open(paths[i], O_RDONLY);
        if (fd < 0) continue;
        ssize_t n;
        while ((n = read(fd, buf, cap)) > 0) mine += (unsigned long long)n;
        close(fd);
    }
    atomic_fetch_add(&total, mine);
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: rawread LIST THREADS\n");
        return 2;
    }
    FILE *f = fopen(argv[1], "rb");
    if (!f) return 2;
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *data = malloc((size_t)len + 1);
    if (fread(data, 1, (size_t)len, f) != (size_t)len) return 2;
    fclose(f);
    data[len] = 0;
    size_t cap = 1024;
    paths = malloc(cap * sizeof(char *));
    for (char *p = data; p < data + len; p += strlen(p) + 1) {
        if (!*p) continue;
        if (npaths == cap) paths = realloc(paths, (cap *= 2) * sizeof(char *));
        paths[npaths++] = p;
    }
    int threads = atoi(argv[2]);
    if (threads < 1) threads = 1;
    pthread_t *t = malloc((size_t)threads * sizeof(pthread_t));
    for (int i = 0; i < threads; i++) pthread_create(&t[i], NULL, worker, NULL);
    for (int i = 0; i < threads; i++) pthread_join(t[i], NULL);
    printf("%llu bytes in %zu files\n", (unsigned long long)atomic_load(&total), npaths);
    return 0;
}
