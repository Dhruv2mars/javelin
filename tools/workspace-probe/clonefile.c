#include <sys/clonefile.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: clonefile source destination\n");
        return 2;
    }
    if (clonefile(argv[1], argv[2], CLONE_NOFOLLOW) != 0) {
        fprintf(stderr, "clonefile errno=%d: %s\n", errno, strerror(errno));
        return 1;
    }
    return 0;
}
