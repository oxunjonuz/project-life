/* bind_deny — make one syscall fail with one errno, then exec. No dependencies, no privileges.
 *
 *   cc -O2 -o bind_deny tools/bind_deny.c
 *   ./bind_deny [--errno N] [--syscall NAME] PROG [ARGS...]
 *
 * Why it exists: on the owner's Mac the interface server was refused a listening socket
 * ("cannot listen on 127.0.0.1:0: Operation not permitted") and that machine is not reachable from
 * here. A seccomp-BPF filter lets the *kernel* produce that exact refusal on any Linux box, so the
 * app's answer to it — the port ladder, the report, the handoff, the log — can be measured rather
 * than asserted. The errno comes from the kernel, not from a stub of mine, which is the difference
 * between a real test and a rehearsal.
 *
 * Default: `bind` fails with EPERM. The architecture is checked first, so an unexpected ABI is left
 * alone instead of killed.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

#if defined(__aarch64__)
#define ARCH_AUDIT AUDIT_ARCH_AARCH64
#define NR_BIND 200
#define NR_SOCKET 198
#elif defined(__x86_64__)
#define ARCH_AUDIT AUDIT_ARCH_X86_64
#define NR_BIND 49
#define NR_SOCKET 41
#else
#error "bind_deny: unsupported architecture"
#endif

static int arch_ok(void) {
    return 1; /* compiled for the architecture we are running on */
}

int main(int argc, char **argv) {
    int err = EPERM;
    int nr = NR_BIND;
    int i = 1;
    while (i < argc && argv[i][0] == '-') {
        if (strcmp(argv[i], "--errno") == 0 && i + 1 < argc) {
            err = atoi(argv[++i]);
        } else if (strcmp(argv[i], "--syscall") == 0 && i + 1 < argc) {
            const char *n = argv[++i];
            if (strcmp(n, "bind") == 0) nr = NR_BIND;
            else if (strcmp(n, "socket") == 0) nr = NR_SOCKET;
            else { fprintf(stderr, "bind_deny: unknown syscall %s\n", n); return 2; }
        } else if (strcmp(argv[i], "--") == 0) {
            i++;
            break;
        } else {
            fprintf(stderr, "bind_deny: unknown option %s\n", argv[i]);
            return 2;
        }
        i++;
    }
    if (i >= argc) {
        fprintf(stderr, "usage: bind_deny [--errno N] [--syscall bind|socket] PROG [ARGS...]\n");
        return 2;
    }

    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, ARCH_AUDIT, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (unsigned)nr, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (err & SECCOMP_RET_DATA)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {
        .len = (unsigned short)(sizeof(filter) / sizeof(filter[0])),
        .filter = filter,
    };
    if (!arch_ok()) return 2;
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        perror("bind_deny: PR_SET_NO_NEW_PRIVS");
        return 2;
    }
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog) != 0) {
        perror("bind_deny: PR_SET_SECCOMP (is seccomp available?)");
        return 2;
    }
    /* A first check, in this process, that the filter really bites: the measured claim is that the
     * kernel refuses the call, so the tool proves it before it is used as evidence. */
    errno = 0;
    int probe = socket(AF_INET, SOCK_STREAM, 0);
    if (nr == NR_SOCKET) {
        if (probe >= 0) {
            fprintf(stderr, "bind_deny: socket() was NOT refused by the filter\n");
            return 3;
        }
        if (errno != err) {
            fprintf(stderr, "bind_deny: socket() failed with errno %d, expected %d\n", errno, err);
            return 3;
        }
        fprintf(stderr, "bind_deny: socket() refused by the kernel with errno %d (%.60s)\n", errno, strerror(errno));
    } else {
        if (probe < 0) {
            fprintf(stderr, "bind_deny: socket() failed unexpectedly: %s\n", strerror(errno));
            return 3;
        }
        struct sockaddr_in a;
        memset(&a, 0, sizeof a);
        a.sin_family = AF_INET;
        a.sin_port = 0;
        a.sin_addr.s_addr = htonl(0x7f000001);
        errno = 0;
        int rc = bind(probe, (struct sockaddr *)&a, sizeof a);
        if (rc == 0 || errno != err) {
            fprintf(stderr, "bind_deny: bind() was NOT refused as intended (rc=%d errno=%d)\n", rc, errno);
            return 3;
        }
        fprintf(stderr, "bind_deny: bind() refused by the kernel with errno %d (%.60s)\n", errno, strerror(errno));
        close(probe);
    }
    fflush(stderr);
    execvp(argv[i], &argv[i]);
    perror("bind_deny: execvp");
    return 2;
}
