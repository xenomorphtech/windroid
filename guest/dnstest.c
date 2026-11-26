#include <netdb.h>
#include <stdio.h>
#include <string.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
int main(int argc, char **argv) {
    const char *host = argc > 1 ? argv[1] : "example.com";
    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof hints);
    hints.ai_socktype = SOCK_STREAM;
    int r = getaddrinfo(host, "80", &hints, &res);
    if (r) { printf("getaddrinfo(%s): %d %s\n", host, r, gai_strerror(r)); return 1; }
    for (struct addrinfo *a = res; a; a = a->ai_next) {
        char buf[64];
        void *p = a->ai_family == AF_INET ? (void *)&((struct sockaddr_in *)a->ai_addr)->sin_addr : (void *)&((struct sockaddr_in6 *)a->ai_addr)->sin6_addr;
        printf("%s -> %s (family %d)\n", host, inet_ntop(a->ai_family, p, buf, sizeof buf), a->ai_family);
    }
    int s = socket(res->ai_family, SOCK_STREAM, 0);
    if (connect(s, res->ai_addr, res->ai_addrlen)) { perror("connect"); return 1; }
    const char *req = "HEAD / HTTP/1.0\r\nHost: example.com\r\n\r\n";
    write(s, req, strlen(req));
    char buf[256]; int n = read(s, buf, sizeof buf - 1);
    if (n > 0) { buf[n] = 0; printf("reply: %.40s\n", buf); }
    close(s);
    freeaddrinfo(res);
    return 0;
}
