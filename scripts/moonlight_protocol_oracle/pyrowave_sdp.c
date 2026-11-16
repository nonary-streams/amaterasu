/* Test-only generator adapter. Link Moonlight SdpGenerator.c; no connection or devices. */
#include "Limelight-internal.h"
#include "ControllerSpeakerPcm.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// Exercise the production SDP generator without starting a connection.
int AppVersionQuad[4] = {7, 1, 431, -1};
STREAM_CONFIGURATION StreamConfig;
CONNECTION_LISTENER_CALLBACKS ListenerCallbacks;
DECODER_RENDERER_CALLBACKS VideoCallbacks;
AUDIO_RENDERER_CALLBACKS AudioCallbacks;
ConnListenerControllerSpeakerPcm ControllerSpeakerPcmCallback;
int NegotiatedVideoFormat;
struct sockaddr_storage RemoteAddr;
uint16_t RtspPortNumber = 48010;
uint16_t VideoPortNumber = 47998;
uint32_t EncryptionFeaturesSupported;
uint32_t EncryptionFeaturesRequested;
uint32_t EncryptionFeaturesEnabled;
bool AudioEncryptionEnabled;
bool HighQualitySurroundSupported;
bool HighQualitySurroundEnabled;
int AudioPacketDuration;

bool PltSafeStrcpy(char* dest, size_t size, const char* src) {
    int written = snprintf(dest, size, "%s", src);
    return written >= 0 && (size_t)written < size;
}

void addrToUrlSafeString(struct sockaddr_storage* addr, char* string, size_t size) {
    (void)addr;
    if (!PltSafeStrcpy(string, size, "127.0.0.1")) abort();
}

bool isReferenceFrameInvalidationSupportedByDecoder(void) { return false; }

int main(void) {
    StreamConfig.width = 2560;
    StreamConfig.height = 1440;
    StreamConfig.fps = 120;
    StreamConfig.bitrate = 695000;
    StreamConfig.packetSize = 1392;
    StreamConfig.streamingRemotely = STREAM_CFG_LOCAL;
    StreamConfig.audioConfiguration = AUDIO_CONFIGURATION_STEREO;
    StreamConfig.encryptionFlags = ENCFLG_ALL;
    StreamConfig.pyrowaveLinkMbps = 1000;
    NegotiatedVideoFormat = VIDEO_FORMAT_PYROWAVE_HDR10;
    EncryptionFeaturesSupported = EncryptionFeaturesRequested = SS_ENC_CONTROL_V2 | SS_ENC_VIDEO | SS_ENC_AUDIO;
    RemoteAddr.ss_family = AF_INET;
    int length = 0;
    char *sdp = getSdpPayloadForStreamConfig(14, &length);
    if (!sdp) return 1;
    size_t written = fwrite(sdp, 1, length, stdout);
    free(sdp);
    return written == (size_t)length ? 0 : 1;
}
