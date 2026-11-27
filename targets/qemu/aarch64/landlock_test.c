#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <dirent.h>
#include <string.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#define CHECK(e)                                                                                             \
    do {                                                                                                     \
        if (!(e)) {                                                                                          \
            dprintf(2, "FAIL line %d (%d)\n", __LINE__, errno);                                              \
            _exit(1);                                                                                        \
        }                                                                                                    \
    } while (0)
enum { READ_FILE = 4, READ_DIR = 8, WRITE_FILE = 2, MAKE_REG = 256 };
static int add(int policy, const char *path, unsigned rights) {
    int dir = open(path, O_PATH | O_CLOEXEC);
    CHECK(dir >= 0);
    struct {
        uint64_t allowed;
        int parent;
    } rule = {rights, dir};
    int rc = syscall(445, policy, 1, &rule, 0);
    close(dir);
    return rc;
}
static void *inherited(void *unused) {
    (void)unused;
    errno = 0;
    CHECK(open("/tmp/denied/file", O_RDONLY) < 0 && errno == EACCES);
    int f = open("/tmp/allow/file", O_RDONLY);
    CHECK(f >= 0);
    close(f);
    return NULL;
}
static int create(const char *p) {
    int fd = open(p, O_RDWR | O_CREAT, 0600);
    CHECK(fd >= 0);
    return fd;
}
static pthread_barrier_t started, scanned;
static void *hold_thread(void *unused) {
    (void)unused;
    pthread_barrier_wait(&started);
    pthread_barrier_wait(&scanned);
    return NULL;
}
static void check_proc(void) {
    pthread_t t;
    CHECK(!pthread_barrier_init(&started, NULL, 2));
    CHECK(!pthread_barrier_init(&scanned, NULL, 2));
    CHECK(!pthread_create(&t, NULL, hold_thread, NULL));
    pthread_barrier_wait(&started);
    DIR *dir = opendir("/proc/self/task");
    CHECK(dir);
    int count = 0;
    struct dirent *e;
    while ((e = readdir(dir))) if (e->d_name[0] != '.') { CHECK(e->d_type == DT_DIR); count++; }
    CHECK(count == 2);
    int relative = openat(dirfd(dir), ".", O_DIRECTORY);
    CHECK(relative >= 0); close(relative);
    closedir(dir);
    dir = opendir("/proc/self/fd"); CHECK(dir);
    int found = 0; char name[30], target[32];
    snprintf(name, sizeof(name), "%d", dirfd(dir));
    while ((e = readdir(dir))) if (!strcmp(e->d_name, name)) { CHECK(e->d_type == DT_LNK); found++; }
    CHECK(found == 1);
    CHECK(readlinkat(dirfd(dir), name, target, sizeof(target)) == (ssize_t)strlen("/proc/self/fd"));
    closedir(dir);
    dir = opendir("/dev"); CHECK(dir);
    found = 0;
    while ((e = readdir(dir))) if (!strcmp(e->d_name, "null")) { CHECK(e->d_type == DT_CHR); found++; }
    CHECK(found == 1); closedir(dir);
    pthread_barrier_wait(&scanned);
    pthread_join(t,NULL);
}
int main(void) {
    check_proc();
    CHECK(syscall(444, NULL, 0, 1) == 1);
    CHECK(!mkdir("/tmp/allow", 0700) && !mkdir("/tmp/denied", 0700) && !mkdir("/tmp/alias", 0700));
    int fa = create("/tmp/allow/file"), fb = create("/tmp/denied/file");
    close(fa);
    int fx = create("/tmp/alias/exact");
    close(fx);
    int retained = open("/tmp/alias/exact", O_PATH);
    uint64_t rights = 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 512 | 1024 | 2048 | 4096;
    int policy = syscall(444, &rights, 8, 0);
    CHECK(policy >= 0);
    CHECK(!add(policy, "/tmp/allow", READ_FILE | READ_DIR | WRITE_FILE | MAKE_REG | 32));
    CHECK(!add(policy, "/tmp/alias/exact", READ_FILE));
    CHECK(!unlink("/tmp/alias/exact"));
    close(create("/tmp/alias/exact"));
    CHECK(syscall(446, policy, 0) < 0 && errno == EPERM);
    CHECK(!prctl(PR_SET_NO_NEW_PRIVS, 1L, 0L, 0L, 0L));
    CHECK(!syscall(446, policy, 0));
    CHECK(write(fb, "x", 1) == 1);
    close(fb);
    CHECK(open("/tmp/alias/exact", O_RDONLY) < 0 && errno == EACCES);
    CHECK(!add(policy, "/tmp/denied", READ_FILE)); // cannot alter an enforced snapshot
    close(policy);
    close(retained);
    errno = 0;
    CHECK(open("/tmp/denied/file", O_RDONLY) < 0 && errno == EACCES);
    CHECK(open("/dev/null", O_RDONLY) < 0 && errno == EACCES);
    CHECK(open("/dev/ptmx", O_RDWR) < 0 && errno == EACCES);
    CHECK(open("/dev/violet/screen", O_RDWR) < 0 && errno == EACCES);
    int op = open("/tmp/denied/file", O_PATH);
    CHECK(op >= 0);
    char c;
    CHECK(read(op, &c, 1) < 0 && errno == EBADF);
    CHECK(mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, op, 0) == MAP_FAILED && errno == EBADF);
    close(op);
    CHECK(open("/tmp/denied/new", O_CREAT | O_RDWR, 0600) < 0 && errno == EACCES);
    int dir = open("/tmp/allow", O_DIRECTORY);
    CHECK(dir >= 0);
    int fd = openat(dir, "new", O_RDWR | O_CREAT, 0600);
    CHECK(fd >= 0);
    close(fd);
    CHECK(rename("/tmp/allow/new", "/tmp/allow/renamed") == 0);
    CHECK(unlinkat(dir, "renamed", 0) == 0);
    CHECK(mkdir("/tmp/allow/newdir", 0700) < 0 && errno == EACCES);
    CHECK(openat(dir, "../denied/file", O_RDONLY) < 0 && errno == EACCES);
    close(dir);
    pthread_t t;
    CHECK(!pthread_create(&t, NULL, inherited, NULL));
    CHECK(!pthread_join(t, NULL));
    int child = fork();
    CHECK(child >= 0);
    if (!child) {
        inherited(NULL);
        char *argv[] = {"/apps/program", NULL};
        CHECK(execv(argv[0], argv) < 0 && errno == EACCES);
        _exit(0);
    }
    int status;
    CHECK(waitpid(child, &status, 0) == child && !status);
    rights = READ_FILE;
    int only = syscall(444, &rights, 8, 0);
    CHECK(only >= 0);
    CHECK(!add(only, "/tmp/denied", READ_FILE));
    CHECK(!syscall(446, only, 0));
    close(only);
    CHECK(open("/tmp/allow/file", O_RDONLY) < 0 && errno == EACCES);
    CHECK(open("/tmp/denied/file", O_RDONLY) < 0 && errno == EACCES);
    puts("Violet Landlock: allowed subtree, denied escapes, inode replacement, inherited and stacked rules "
         "passed");
}
