#!/bin/sh
# Build an explicit existing upstream checkout; no automatic downloads.
set -eu
enet_source=${1:?usage: build-oracle.sh ENET_CHECKOUT OUTPUT_EXECUTABLE}
oracle_output=${2:?usage: build-oracle.sh ENET_CHECKOUT OUTPUT_EXECUTABLE}
oracle_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "$(git -C "$enet_source" rev-parse HEAD)" != aca87840b57f045a1f7f9299e4b1b9b8e2a5e2f1 ]; then
    echo "unexpected ENet commit" >&2
    exit 1
fi
git -C "$enet_source" diff --quiet HEAD --
cc -std=c11 -O2 -DHAS_GETADDRINFO=1 -DHAS_GETNAMEINFO=1 \
    -DHAS_POLL=1 -DHAS_FCNTL=1 -DHAS_INET_PTON=1 -DHAS_INET_NTOP=1 \
    -DHAS_MSGHDR_FLAGS=1 -DHAS_SOCKLEN_T=1 -I"$enet_source/include" \
    "$oracle_directory/oracle.c" "$enet_source/callbacks.c" \
    "$enet_source/compress.c" "$enet_source/host.c" "$enet_source/list.c" \
    "$enet_source/packet.c" "$enet_source/peer.c" "$enet_source/protocol.c" \
    "$enet_source/unix.c" -o "$oracle_output"
