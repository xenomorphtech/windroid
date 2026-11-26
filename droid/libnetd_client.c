// Replacement for Android's libnetd_client.so.
//
// libc dlopen()s this library and calls the netdClientInit* hooks to route
// sockets and DNS through netd. There is no netd here: sockets go straight to
// the kernel (wwd's Winsock layer), and DNS goes to /dev/socket/dnsproxyd,
// which wwd serves itself (crates/wwd-linux/src/net.rs) with the host resolver.
#include <fcntl.h>
#include <unistd.h>

typedef int (*DnsOpenProxyType)(void);

static int dns_open_proxy(void) {
    return open("/dev/socket/dnsproxyd", O_RDWR | O_CLOEXEC);
}

void netdClientInitDnsOpenProxy(DnsOpenProxyType *function) {
    if (function) *function = dns_open_proxy;
}

// The other hooks keep libc's defaults (plain syscalls, netid passthrough).
void netdClientInitAccept4(void *function) {}
void netdClientInitConnect(void *function) {}
void netdClientInitSocket(void *function) {}
void netdClientInitSendmmsg(void *function) {}
void netdClientInitSendmsg(void *function) {}
void netdClientInitSendto(void *function) {}
void netdClientInitNetIdForResolv(void *function) {}
