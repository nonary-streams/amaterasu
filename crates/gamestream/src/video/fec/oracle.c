/* Test-only bridge to an external, pinned Moonlight nanors checkout.
 * No upstream implementation is embedded in this repository. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "rs.h"

int main(int argc, char **argv) {
    if (argc != 5) return 2;
    int data = atoi(argv[2]), parity = atoi(argv[3]), bytes = atoi(argv[4]);
    if (data < 1 || parity < 1 || data + parity > 255 || bytes < 1 || bytes > 65507) return 2;
    int total = data + parity;
    unsigned char **shards = calloc((size_t)total, sizeof(*shards));
    unsigned char *marks = calloc((size_t)total, 1);
    reed_solomon *codec = reed_solomon_new(data, parity);
    if (!shards || !marks || !codec) return 3;
    for (int index = 0; index < total; index++) {
        shards[index] = calloc((size_t)bytes, 1);
        if (!shards[index]) return 3;
    }
    const char *mode = argv[1];
    if (strncmp(mode, "audio-", 6) == 0) {
        if (data != 4 || parity != 2) return 2;
        /* Fixed protocol matrix installed by Moonlight RtpAudioQueue itself. */
        const unsigned char audio_matrix[8] = {0x77,0x40,0x38,0x0e,0xc7,0xa7,0x0d,0x6c};
        memcpy(codec->p, audio_matrix, sizeof(audio_matrix));
        mode += 6;
    }
    int result;
    if (strcmp(mode, "encode") == 0) {
        for (int index = 0; index < data; index++) {
            if (fread(shards[index], 1, (size_t)bytes, stdin) != (size_t)bytes) return 4;
        }
        result = reed_solomon_encode(codec, shards, total, bytes);
        if (result != 0) return 5;
        for (int index = data; index < total; index++) {
            if (fwrite(shards[index], 1, (size_t)bytes, stdout) != (size_t)bytes) return 6;
        }
    } else if (strcmp(mode, "decode") == 0) {
        if (fread(marks, 1, (size_t)total, stdin) != (size_t)total) return 4;
        for (int index = 0; index < total; index++) {
            if (fread(shards[index], 1, (size_t)bytes, stdin) != (size_t)bytes) return 4;
        }
        result = reed_solomon_decode(codec, shards, marks, total, bytes);
        if (result != 0) return 5;
        for (int index = 0; index < data; index++) {
            if (fwrite(shards[index], 1, (size_t)bytes, stdout) != (size_t)bytes) return 6;
        }
    } else return 2;
    for (int index = 0; index < total; index++) free(shards[index]);
    free(shards);
    free(marks);
    reed_solomon_release(codec);
    return 0;
}
