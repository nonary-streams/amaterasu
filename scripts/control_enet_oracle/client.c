#define _POSIX_C_SOURCE 200809L
#include <enet/enet.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define MAX_PAYLOAD 65539u
#define DEADLINE_MS 5000u
#define INTERACTIVE_DEADLINE_MS 30000u
#define MAX_INPUT_PACKETS 64u

struct input_stream {
    unsigned char bytes[MAX_PAYLOAD + 4];
    size_t used;
    unsigned int sent;
};

static uint64_t now_ms(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)value.tv_sec * 1000 + (uint64_t)value.tv_nsec / 1000000;
}

static int parse_number(const char *text, uint32_t maximum, uint32_t *result) {
    if (*text == '\0') return -1;
    uint64_t value = 0;
    for (const unsigned char *p = (const unsigned char *)text; *p; ++p) {
        if (*p < '0' || *p > '9') return -1;
        value = value * 10 + (*p - '0');
        if (value > maximum) return -1;
    }
    *result = (uint32_t)value;
    return 0;
}

static int read_payload(const char *path, unsigned char *bytes, size_t *length) {
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    if (fd < 0) { perror("open payload"); return -1; }
    struct stat info;
    if (fstat(fd, &info) != 0 || !S_ISREG(info.st_mode)) {
        fprintf(stderr, "payload must be a regular file\n");
        close(fd);
        return -1;
    }
    *length = 0;
    while (*length <= MAX_PAYLOAD) {
        ssize_t count = read(fd, bytes + *length, MAX_PAYLOAD + 1 - *length);
        if (count < 0 && errno == EINTR) continue;
        if (count < 0) { perror("read payload"); close(fd); return -1; }
        if (count == 0) break;
        *length += (size_t)count;
    }
    close(fd);
    if (*length == 0 || *length > MAX_PAYLOAD) {
        fprintf(stderr, "payload must contain 1..65539 bytes\n");
        return -1;
    }
    return 0;
}

static int write_output(const char *bytes, size_t length, uint64_t deadline) {
    while (length > 0 && now_ms() < deadline) {
        ssize_t count = write(STDOUT_FILENO, bytes, length);
        if (count > 0) { bytes += count; length -= (size_t)count; continue; }
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct pollfd descriptor = {STDOUT_FILENO, POLLOUT, 0};
            if (poll(&descriptor, 1, 20) < 0 && errno != EINTR) return -1;
            continue;
        }
        return -1;
    }
    return length == 0 ? 0 : -1;
}

static int print_packet(const ENetPacket *packet, uint64_t deadline) {
    static const char digits[] = "0123456789abcdef";
    if (packet->dataLength > MAX_PAYLOAD) {
        fprintf(stderr, "reply exceeds payload bound\n");
        return -1;
    }
    char *line = malloc(10 + packet->dataLength * 2);
    if (!line) return -1;
    memcpy(line, "RECEIVED ", 9);
    for (size_t i = 0; i < packet->dataLength; ++i) {
        line[9 + i * 2] = digits[packet->data[i] >> 4];
        line[10 + i * 2] = digits[packet->data[i] & 15];
    }
    line[9 + packet->dataLength * 2] = '\n';
    int result = write_output(line, 10 + packet->dataLength * 2, deadline);
    free(line);
    return result;
}

static int send_packet(ENetHost *host, ENetPeer *peer,
                       const unsigned char *bytes, size_t length) {
    ENetPacket *packet = enet_packet_create(bytes, length, ENET_PACKET_FLAG_RELIABLE);
    if (!packet) return -1;
    if (enet_peer_send(peer, 2, packet) != 0) {
        enet_packet_destroy(packet);
        fprintf(stderr, "ENet send failed\n");
        return -1;
    }
    enet_host_flush(host);
    return 0;
}

/* Interactive input uses a big-endian u32 length followed by one encrypted
 * packet. Retain partial pipe reads and accept at most 64 packets per process. */
static int poll_input(struct input_stream *input, ENetHost *host,
                      ENetPeer *peer, uint64_t deadline) {
    size_t required = 4;
    if (input->used >= 4) {
        uint32_t length = ((uint32_t)input->bytes[0] << 24) |
                          ((uint32_t)input->bytes[1] << 16) |
                          ((uint32_t)input->bytes[2] << 8) | input->bytes[3];
        if (length == 0 || length > MAX_PAYLOAD) return -1;
        required += length;
    }
    ssize_t count = read(STDIN_FILENO, input->bytes + input->used,
                         required - input->used);
    if (count < 0) {
        return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR ? 0 : -1;
    }
    if (count == 0) {
        if (input->used != 0) return -1;
        return 1;
    }
    input->used += (size_t)count;
    if (required == 4 || input->used < required) return 0;
    if (input->sent >= MAX_INPUT_PACKETS ||
        send_packet(host, peer, input->bytes + 4, required - 4) != 0) return -1;
    input->sent++;
    input->used = 0;
    return write_output("SENT\n", 5, deadline);
}

static int exchange(ENetHost *host, ENetPeer *peer, const unsigned char *bytes,
                    size_t length, uint64_t deadline, int interactive) {
    int connected = 0;
    struct input_stream input = {0};
    while (now_ms() < deadline) {
        if (interactive && connected) {
            int result = poll_input(&input, host, peer, deadline);
            if (result != 0) return result == 1 ? 0 : -1;
        }
        ENetEvent event;
        int received = enet_host_service(host, &event, 20);
        if (received < 0) { fprintf(stderr, "ENet service failed\n"); return -1; }
        if (received == 0) continue;
        if (event.type == ENET_EVENT_TYPE_CONNECT && event.peer == peer && !connected) {
            connected = 1;
            if (write_output("CONNECTED\n", 10, deadline) != 0) return -1;
            if (send_packet(host, peer, bytes, length) != 0) return -1;
        } else if (event.type == ENET_EVENT_TYPE_RECEIVE) {
            int valid = connected && event.peer == peer && event.channelID == 2;
            enet_host_flush(host);
            int result = valid ? print_packet(event.packet, deadline) : -1;
            enet_packet_destroy(event.packet);
            if (!valid) fprintf(stderr, "unexpected reply peer/channel\n");
            if (result != 0 || !interactive) return result;
        } else if (event.type == ENET_EVENT_TYPE_DISCONNECT) {
            fprintf(stderr, "peer disconnected before reply\n");
            return -1;
        }
    }
    fprintf(stderr, "exchange deadline expired (%s)\n", connected ? "reply" : "connect");
    return -1;
}

int main(int argc, char **argv) {
    uint32_t port, connect_data;
    int interactive = argc == 5 && strcmp(argv[4], "--interactive") == 0;
    if ((argc != 4 && !interactive) || parse_number(argv[1], 65535, &port) != 0 || port == 0 ||
        parse_number(argv[2], UINT32_MAX, &connect_data) != 0) {
        fprintf(stderr, "usage: %s PORT CONNECT_DATA PAYLOAD_FILE [--interactive]\n", argv[0]);
        return 2;
    }
    unsigned char bytes[MAX_PAYLOAD + 1];
    size_t length;
    if (read_payload(argv[3], bytes, &length) != 0) return 2;
    signal(SIGPIPE, SIG_IGN);
    int flags = fcntl(STDOUT_FILENO, F_GETFL);
    if (flags < 0 || fcntl(STDOUT_FILENO, F_SETFL, flags | O_NONBLOCK) != 0) return 1;
    if (interactive) {
        flags = fcntl(STDIN_FILENO, F_GETFL);
        if (flags < 0 || fcntl(STDIN_FILENO, F_SETFL, flags | O_NONBLOCK) != 0) return 1;
    }
    if (enet_initialize() != 0) return 1;
    ENetHost *host = enet_host_create(AF_INET, NULL, 1, 48, 0, 0);
    if (!host) { enet_deinitialize(); return 1; }
    host->maximumPacketSize = MAX_PAYLOAD;
    host->maximumWaitingData = MAX_PAYLOAD * 4;
    struct sockaddr_in socket_address = {0};
    socket_address.sin_family = AF_INET;
    socket_address.sin_port = htons((uint16_t)port);
    (void)inet_pton(AF_INET, "127.0.0.1", &socket_address.sin_addr);
    ENetAddress address;
    int result = -1;
    if (enet_address_set_address(&address, (struct sockaddr *)&socket_address,
                                 sizeof(socket_address)) == 0) {
        ENetPeer *peer = enet_host_connect(host, &address, 48, connect_data);
        if (peer) {
            uint64_t duration = interactive ? INTERACTIVE_DEADLINE_MS : DEADLINE_MS;
            result = exchange(host, peer, bytes, length, now_ms() + duration, interactive);
            enet_peer_reset(peer);
        }
    }
    enet_host_destroy(host);
    enet_deinitialize();
    if (result != 0) fprintf(stderr, "oracle exchange failed\n");
    return result == 0 ? 0 : 1;
}
