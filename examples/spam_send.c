/*
 * spam_send — flood sends as fast as possible at a peer that never answers.
 * Under --window with --window-ops N, buffering more than N sends inside one
 * window is the F7 overflow: the run must abort-and-discard (exit 3) with a
 * latched violation instead of growing the window buffer without bound.
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

    struct sockaddr_in me;
    memset(&me, 0, sizeof me);
    me.sin_family = AF_INET;
    me.sin_port = htons(9000);
    me.sin_addr.s_addr = htonl(0x7f000001u + (unsigned)node);
    if (bind(fd, (struct sockaddr *)&me, sizeof me) < 0) { perror("bind"); return 2; }

    struct sockaddr_in peer;
    memset(&peer, 0, sizeof peer);
    peer.sin_family = AF_INET;
    peer.sin_port = htons(9001);
    peer.sin_addr.s_addr = htonl(0x7f000002u);

    char msg[16] = "spam";
    for (int i = 0; i < 2000; i++) {
        sendto(fd, msg, sizeof msg, 0, (struct sockaddr *)&peer, sizeof peer);
    }
    return 0;
}
