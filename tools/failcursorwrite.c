/* Fails write(2) calls whose buffer is the CURSOR DOCUMENT, with ENOSPC.
 *
 * Build:  gcc -shared -fPIC -o failcursorwrite.so failcursorwrite.c -ldl
 * Use:    LD_PRELOAD=./failcursorwrite.so <cmd>
 *
 * Why content-matched and not path-matched: the fault must land on the
 * cursor write in BOTH the pre-fix tree (a direct write to the cursor
 * path) and the fixed tree (a write to a sibling temp file). The cursor
 * document has a fixed shape — one line of JSON starting with {"v":1, — so
 * matching the buffer prefix hits exactly that write and nothing else.
 *
 * The errno is ENOSPC: the real condition this host actually hits (the
 * build machine ran at 100% full twice in two days).
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static ssize_t (*real_write)(int, const void *, size_t);
static int hits_remaining = -1;

ssize_t write(int fd, const void *buf, size_t count) {
    if (!real_write) real_write = dlsym(RTLD_NEXT, "write");
    if (hits_remaining == -1) {
        const char *t = getenv("FAILCURSOR_TIMES");
        hits_remaining = t ? atoi(t) : 1;
    }
    if (hits_remaining > 0 && count >= 7 &&
        memcmp(buf, "{\"v\":1,", 7) == 0) {
        hits_remaining--;
        errno = ENOSPC;
        return -1;
    }
    return real_write(fd, buf, count);
}
