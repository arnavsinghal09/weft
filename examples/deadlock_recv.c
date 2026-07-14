/*
 * deadlock_recv — bind a UDP socket and block in recvfrom for a datagram that
 * never arrives. A single node here has no peer, so under the windowed broker
 * the cluster reaches terminal quiescence: nothing is in flight and the only
 * connected guest is blocked with nothing that can wake it. The orchestrator
 * must abort-and-discard the run (F6 deterministic deadlock report, and the
 * --watchdog real-time guard for F3) instead of hanging forever.
 *
 * Run: weft run --seed 0 --net "latency=fixed:100" --window 100 -- deadlock_recv
 */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

int main(void) {
    const char *nid = getenv("WEFT_NODE_ID");
    int node = nid ? atoi(nid) : 0;

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { perror("socket"); return 2; }

    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(9000);
    a.sin_addr.s_addr = htonl(0x7f000001u + (unsigned)node);
    if (bind(fd, (struct sockaddr *)&a, sizeof a) < 0) { perror("bind"); return 2; }

    char buf[64];
    /* Never returns: no peer will ever send to this address. */
    recvfrom(fd, buf, sizeof buf, 0, NULL, NULL);
    return 0;
}
