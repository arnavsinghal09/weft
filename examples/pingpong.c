/* pingpong: the minimal two-process proof that simulated networking works.
 *
 * Run with: weft run --seed S --net "" --nodes 2 -- pingpong
 *
 * Node 0 binds 127.0.0.1:4000, waits for a PING, replies PONG:<payload>.
 * Node 1 binds 127.0.0.2:4001, sends PING:<value derived from getrandom>,
 * waits for the PONG and prints it. Both sides print what they saw, so the
 * combined output proves: datagrams crossed process boundaries, went through
 * the broker (not the kernel UDP stack), and carried seed-deterministic
 * content.
 */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <unistd.h>

static struct sockaddr_in mkaddr(const char *ip, int port) {
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((unsigned short)port);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}

int main(void) {
    const char *nid = getenv("WEFT_NODE_ID");
    int node = nid ? atoi(nid) : 0;

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { perror("socket"); return 2; }

    if (node == 0) {
        struct sockaddr_in me = mkaddr("127.0.0.1", 4000);
        if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("bind"); return 2; }
        char buf[256];
        struct sockaddr_in from; socklen_t flen = sizeof from;
        ssize_t n = recvfrom(fd, buf, sizeof buf - 1, 0, (struct sockaddr *)&from, &flen);
        if (n < 0) { perror("recvfrom"); return 2; }
        buf[n] = 0;
        printf("server got: %s\n", buf);
        char reply[300];
        int m = snprintf(reply, sizeof reply, "PONG:%s", buf + 5);
        sendto(fd, reply, (size_t)m, 0, (struct sockaddr *)&from, flen);
    } else {
        struct sockaddr_in me = mkaddr("127.0.0.2", 4001);
        if (bind(fd, (struct sockaddr *)&me, sizeof me) != 0) { perror("bind"); return 2; }
        unsigned long long r = 0;
        getrandom(&r, sizeof r, 0); /* seed-deterministic payload */
        char msg[64];
        int m = snprintf(msg, sizeof msg, "PING:%016llx", r);
        struct sockaddr_in srv = mkaddr("127.0.0.1", 4000);
        /* The server may not have bound yet: a datagram to an unbound address
         * is discarded (standard UDP semantics, which the simulation keeps).
         * So do what every real UDP client does — retry until answered. */
        char buf[256];
        ssize_t n;
        for (;;) {
            sendto(fd, msg, (size_t)m, 0, (struct sockaddr *)&srv, sizeof srv);
            int waited;
            for (waited = 0; waited < 200; waited++) {
                n = recvfrom(fd, buf, sizeof buf - 1, MSG_DONTWAIT, NULL, NULL);
                if (n >= 0) goto answered;
            }
        }
    answered:
        buf[n] = 0;
        printf("client got: %s\n", buf);
    }
    close(fd);
    return 0;
}
