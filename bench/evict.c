/* Drop the page-cache pages of a list of files without root: map each file and msync it with
 * MS_INVALIDATE, then report how many pages are still resident (mincore). Used as the cold-cache
 * preparation step on machines where `purge` / `drop_caches` need privileges.
 *
 *   cc -O2 -o evict bench/evict.c
 *   evict LIST [--check]    (LIST: NUL-separated paths relative to the current directory)
 */
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static size_t resident(void *p, size_t len, size_t page) {
    size_t n = (len + page - 1) / page, r = 0;
    char *vec = malloc(n);
    if (!vec || mincore(p, len, (void *)vec) != 0) {
        free(vec);
        return 0;
    }
    for (size_t i = 0; i < n; i++) r += (vec[i] & 1);
    free(vec);
    return r;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: evict LIST [--check]\n");
        return 2;
    }
    int check = argc > 2 && strcmp(argv[2], "--check") == 0;
    FILE *f = fopen(argv[1], "rb");
    if (!f) return 2;
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *data = malloc((size_t)len + 1);
    if (fread(data, 1, (size_t)len, f) != (size_t)len) return 2;
    fclose(f);
    data[len] = 0;
    size_t page = (size_t)sysconf(_SC_PAGESIZE), before = 0, after = 0, total = 0;
    for (char *p = data; p < data + len; p += strlen(p) + 1) {
        if (!*p) continue;
        int fd = open(p, O_RDONLY);
        if (fd < 0) continue;
        struct stat st;
        if (fstat(fd, &st) == 0 && st.st_size > 0) {
            void *m = mmap(NULL, (size_t)st.st_size, PROT_READ, MAP_SHARED, fd, 0);
            if (m != MAP_FAILED) {
                total += ((size_t)st.st_size + page - 1) / page;
                if (check) before += resident(m, (size_t)st.st_size, page);
                msync(m, (size_t)st.st_size, MS_INVALIDATE);
                if (check) after += resident(m, (size_t)st.st_size, page);
                munmap(m, (size_t)st.st_size);
            }
        }
        close(fd);
    }
    if (check) printf("pages: %zu total, %zu resident before, %zu after\n", total, before, after);
    return 0;
}
