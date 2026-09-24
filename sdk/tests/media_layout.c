#include "amaterasu_media.h"
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(AmaterasuNativeInterface) == 32, "native prefix layout");
_Static_assert(sizeof(AmaterasuNativeInterfaceQuery) == 24, "native query layout");
_Static_assert(sizeof(AmaterasuNativeServiceFactory) == 32, "native factory layout");
_Static_assert(sizeof(AmaterasuCpuFrame) == 48, "frame layout");
_Static_assert(offsetof(AmaterasuCpuFrame, data) == 16, "frame data offset");
_Static_assert(offsetof(AmaterasuCpuFrame, timestamp_us) == 32, "frame timestamp offset");
_Static_assert(sizeof(AmaterasuEncodedPacket) == 40, "packet layout");
_Static_assert(sizeof(AmaterasuDesktopRect) == 16, "desktop rectangle layout");
_Static_assert(offsetof(AmaterasuDesktopRect, width) == 8, "desktop extent offset");
_Static_assert(sizeof(AmaterasuCaptureGeometry) == 32, "capture geometry layout");
_Static_assert(offsetof(AmaterasuCaptureGeometry, virtual_desktop) == 16, "virtual desktop offset");
_Static_assert(sizeof(AmaterasuCaptureApiPrefix) == 40, "legacy capture table layout");
_Static_assert(AMATERASU_CAPTURE_API_V1_0_SIZE == 40, "legacy capture minimum size");
_Static_assert(sizeof(AmaterasuCaptureApi) == 48, "capture table layout");
_Static_assert(offsetof(AmaterasuCaptureApi, read_frame) == 32, "capture prefix callback offset");
_Static_assert(offsetof(AmaterasuCaptureApi, geometry) == 40, "capture geometry callback offset");
_Static_assert(AMATERASU_CAPTURE_API_V1_1_SIZE == 48, "capture v1.1 minimum size");
_Static_assert(AMATERASU_CAPTURE_FRESHNESS_INTERFACE_MINOR == 2u, "capture freshness ABI minor");
_Static_assert(sizeof(AmaterasuFreshFrameSinkV1_2) == 16, "fresh capture sink layout");
_Static_assert(sizeof(AmaterasuCaptureApiV1_2) == 56, "capture v1.2 table layout");
_Static_assert(AMATERASU_CAPTURE_API_V1_2_SIZE == 56, "capture v1.2 minimum size");
_Static_assert(offsetof(AmaterasuCaptureApiV1_2, read_frame_with_freshness) == 48, "capture freshness callback offset");
_Static_assert(sizeof(AmaterasuEncoderApi) == 56, "encoder table layout");
_Static_assert(sizeof(AmaterasuVideoApi) == 40, "video table layout");
_Static_assert(sizeof(AmaterasuAudioFrame) == 56, "audio frame layout");
_Static_assert(offsetof(AmaterasuAudioFrame, data) == 24, "audio data offset");
_Static_assert(sizeof(AmaterasuAudioCaptureApi) == 40, "audio table layout");
_Static_assert(sizeof(AmaterasuEncodedAudioPacket) == 56, "encoded audio layout");
_Static_assert(offsetof(AmaterasuEncodedAudioPacket, data) == 24, "encoded audio data offset");
_Static_assert(offsetof(AmaterasuEncodedAudioPacket, timestamp_us) == 40, "encoded audio timestamp offset");
_Static_assert(sizeof(AmaterasuAudioEncoderApi) == 56, "audio encoder table layout");
_Static_assert(offsetof(AmaterasuAudioEncoderApi, submit) == 32, "audio encoder submit offset");
_Static_assert(offsetof(AmaterasuAudioEncoderApi, receive) == 40, "audio encoder receive offset");
_Static_assert(offsetof(AmaterasuAudioEncoderApi, drain) == 48, "audio encoder drain offset");
_Static_assert(sizeof(AmaterasuMediaTransportApi) == 56, "media transport table layout");
_Static_assert(offsetof(AmaterasuMediaTransportApi, submit_video) == 32, "transport video offset");
_Static_assert(offsetof(AmaterasuMediaTransportApi, submit_audio) == 40, "transport audio offset");
_Static_assert(offsetof(AmaterasuMediaTransportApi, drain) == 48, "transport drain offset");
#endif

_Static_assert(sizeof(AmaterasuControlMessage) == 40, "control message layout");
_Static_assert(offsetof(AmaterasuControlMessage, data) == 16, "control data layout");
_Static_assert(offsetof(AmaterasuControlMessage, sequence) == 32, "control sequence layout");
_Static_assert(sizeof(AmaterasuControlTransportApi) == 48, "control API layout");
_Static_assert(offsetof(AmaterasuControlTransportApi, send) == 32, "control send layout");
_Static_assert(offsetof(AmaterasuControlTransportApi, receive) == 40, "control receive layout");
