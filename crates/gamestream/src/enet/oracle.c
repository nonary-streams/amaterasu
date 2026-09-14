/* Original test bridge against an external pinned Moonlight ENet checkout.
 * The actual upstream library supplies both struct layouts and peer behavior. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include "enet/enet.h"

static void dump(const char *name, const void *header, size_t header_size,
                 const void *command, size_t command_size, const void *data, size_t data_size) {
    const unsigned char *parts[] = {header, command, data};
    size_t sizes[] = {header_size, command_size, data_size};
    printf("%s ", name);
    for (size_t part = 0; part < 3; part++) {
        for (size_t byte = 0; byte < sizes[part]; byte++) printf("%02x", parts[part][byte]);
    }
    putchar('\n');
}

static int fixtures(void) {
    ENetProtocolHeader header = {ENET_HOST_TO_NET_16(0x8fff), ENET_HOST_TO_NET_16(0xbeef)};
    ENetProtocolConnect connect = {0};
    connect.header.command = ENET_PROTOCOL_COMMAND_CONNECT | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE;
    connect.header.channelID = 255;
    connect.header.reliableSequenceNumber = ENET_HOST_TO_NET_16(1);
    connect.outgoingPeerID = ENET_HOST_TO_NET_16(7);
    connect.incomingSessionID = connect.outgoingSessionID = 255;
    connect.mtu = ENET_HOST_TO_NET_32(900);
    connect.windowSize = ENET_HOST_TO_NET_32(65536);
    connect.channelCount = ENET_HOST_TO_NET_32(48);
    connect.packetThrottleInterval = ENET_HOST_TO_NET_32(5000);
    connect.packetThrottleAcceleration = connect.packetThrottleDeceleration = ENET_HOST_TO_NET_32(2);
    const unsigned char opaque_id[] = {0x12,0x34,0x56,0x78};
    memcpy(&connect.connectID, opaque_id, 4);
    connect.data = ENET_HOST_TO_NET_32(0x11223344);
    dump("connect", &header, sizeof(header), &connect, sizeof(connect), NULL, 0);
    ENetProtocolVerifyConnect verify;
    memcpy(&verify, &connect, sizeof(verify));
    verify.header.command = ENET_PROTOCOL_COMMAND_VERIFY_CONNECT | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE;
    verify.incomingSessionID = 1;
    verify.outgoingSessionID = 2;
    header.peerID = ENET_HOST_TO_NET_16(0xa007);
    dump("verify", &header, sizeof(header), &verify, sizeof(verify), NULL, 0);
    header.peerID = ENET_HOST_TO_NET_16(0x1007);
    ENetProtocolAcknowledge ack = {{ENET_PROTOCOL_COMMAND_ACKNOWLEDGE, 255, ENET_HOST_TO_NET_16(0xffff)}, ENET_HOST_TO_NET_16(0xffff), ENET_HOST_TO_NET_16(0xbeef)};
    dump("ack", &header, 2, &ack, sizeof(ack), NULL, 0);
    header.peerID = ENET_HOST_TO_NET_16(0x9007);
    ENetProtocolSendReliable reliable = {{ENET_PROTOCOL_COMMAND_SEND_RELIABLE | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE, 7, ENET_HOST_TO_NET_16(0xffff)}, ENET_HOST_TO_NET_16(3)};
    dump("reliable", &header, 4, &reliable, sizeof(reliable), "abc", 3);
    header.peerID = ENET_HOST_TO_NET_16(0x1007);
    ENetProtocolSendUnreliable unreliable = {{ENET_PROTOCOL_COMMAND_SEND_UNRELIABLE, 7, ENET_HOST_TO_NET_16(9)}, ENET_HOST_TO_NET_16(0xffff), ENET_HOST_TO_NET_16(3)};
    dump("unreliable", &header, 2, &unreliable, sizeof(unreliable), "xyz", 3);
    ENetProtocolSendUnsequenced unsequenced = {{ENET_PROTOCOL_COMMAND_SEND_UNSEQUENCED | ENET_PROTOCOL_COMMAND_FLAG_UNSEQUENCED, 7, 0}, ENET_HOST_TO_NET_16(0x1234), ENET_HOST_TO_NET_16(3)};
    dump("unsequenced", &header, 2, &unsequenced, sizeof(unsequenced), "def", 3);
    header.peerID = ENET_HOST_TO_NET_16(0x9007);
    ENetProtocolSendFragment fragment = {{ENET_PROTOCOL_COMMAND_SEND_FRAGMENT | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE, 7, ENET_HOST_TO_NET_16(0xffff)}, ENET_HOST_TO_NET_16(0xffff), ENET_HOST_TO_NET_16(4), ENET_HOST_TO_NET_32(2), 0, ENET_HOST_TO_NET_32(8), 0};
    dump("fragment", &header, 4, &fragment, sizeof(fragment), "abcd", 4);
    header.peerID = ENET_HOST_TO_NET_16(0x1007);
    fragment.header.command = ENET_PROTOCOL_COMMAND_SEND_UNRELIABLE_FRAGMENT;
    fragment.header.reliableSequenceNumber = ENET_HOST_TO_NET_16(9);
    fragment.startSequenceNumber = ENET_HOST_TO_NET_16(99);
    fragment.fragmentNumber = ENET_HOST_TO_NET_32(1);
    fragment.fragmentOffset = ENET_HOST_TO_NET_32(4);
    dump("unreliable_fragment", &header, 2, &fragment, sizeof(fragment), "efgh", 4);
    header.peerID = ENET_HOST_TO_NET_16(0x9007);
    ENetProtocolDisconnect disconnect = {{ENET_PROTOCOL_COMMAND_DISCONNECT | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE,255,ENET_HOST_TO_NET_16(2)},ENET_HOST_TO_NET_32(0x55667788)};
    dump("disconnect", &header,4,&disconnect,sizeof(disconnect),NULL,0);
    ENetProtocolPing ping = {{ENET_PROTOCOL_COMMAND_PING | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE,255,ENET_HOST_TO_NET_16(3)}};
    dump("ping", &header,4,&ping,sizeof(ping),NULL,0);
    ENetProtocolBandwidthLimit bandwidth = {{ENET_PROTOCOL_COMMAND_BANDWIDTH_LIMIT | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE,255,ENET_HOST_TO_NET_16(4)},ENET_HOST_TO_NET_32(12345),ENET_HOST_TO_NET_32(67890)};
    dump("bandwidth", &header,4,&bandwidth,sizeof(bandwidth),NULL,0);
    ENetProtocolThrottleConfigure throttle = {{ENET_PROTOCOL_COMMAND_THROTTLE_CONFIGURE | ENET_PROTOCOL_COMMAND_FLAG_ACKNOWLEDGE,255,ENET_HOST_TO_NET_16(5)},ENET_HOST_TO_NET_32(5000),ENET_HOST_TO_NET_32(2),ENET_HOST_TO_NET_32(2)};
    dump("throttle", &header,4,&throttle,sizeof(throttle),NULL,0);
    return 0;
}

static int client(int port) {
    if (port < 1 || port > 65535 || enet_initialize() != 0) return 2;
    ENetAddress address = {0};
    if (enet_address_set_host(&address,"127.0.0.1") != 0) return 2;
    enet_address_set_port(&address,(enet_uint16)port);
    ENetHost *host = enet_host_create(AF_INET,NULL,1,48,0,0);
    if (!host) return 3;
    ENetPeer *peer = enet_host_connect(host,&address,48,0x11223344);
    if (!peer) return 3;
    unsigned char large[2000];
    for (size_t i = 0; i < sizeof(large); i++) large[i] = (unsigned char)(i*17+5);
    unsigned received = 0;
    int result = 4;
    enet_uint32 start = enet_time_get();
    while (enet_time_get()-start < 5000) {
        ENetEvent event;
        int ready = enet_host_service(host,&event,10);
        if (ready < 0) break;
        if (!ready) continue;
        if (event.type == ENET_EVENT_TYPE_CONNECT) {
            if (enet_peer_send(peer,0,enet_packet_create("enet-oracle",11,ENET_PACKET_FLAG_RELIABLE)) != 0 ||
                enet_peer_send(peer,7,enet_packet_create(large,sizeof(large),ENET_PACKET_FLAG_RELIABLE)) != 0) break;
            puts("connected");
        } else if (event.type == ENET_EVENT_TYPE_RECEIVE) {
            unsigned bit = 0;
            if (event.channelID == 0 && event.packet->dataLength == 11 && memcmp(event.packet->data,"enet-oracle",11) == 0) bit = 1;
            if (event.channelID == 7 && event.packet->dataLength == sizeof(large) && memcmp(event.packet->data,large,sizeof(large)) == 0) bit = 2;
            enet_packet_destroy(event.packet);
            if (!bit || (received & bit)) break;
            received |= bit;
            if (received == 3) enet_peer_disconnect(peer,0x55667788);
        } else if (event.type == ENET_EVENT_TYPE_DISCONNECT) {
            if (received == 3) { puts("reliable-small-and-fragmented-roundtrip-disconnected"); result = 0; }
            break;
        }
    }
    enet_host_destroy(host);
    enet_deinitialize();
    return result;
}

int main(int argc,char **argv) {
    if (argc == 2 && strcmp(argv[1],"fixtures") == 0) return fixtures();
    if (argc == 3 && strcmp(argv[1],"client") == 0) return client(atoi(argv[2]));
    return 2;
}
