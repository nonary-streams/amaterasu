#ifndef AMATERASU_IDENTITY_H
#define AMATERASU_IDENTITY_H
#include "amaterasu.h"
#ifdef __cplusplus
extern "C" {
#endif

#define AMATERASU_IDENTITY_UNAVAILABLE 20
#define AMATERASU_IDENTITY_FORBIDDEN 21
#define AMATERASU_IDENTITY_UNSUPPORTED 22
#define AMATERASU_IDENTITY_UNTRUSTED 23
#define AMATERASU_IDENTITY_MINOR_PERMISSIONS 1
#define AMATERASU_IDENTITY_PERMISSION_LIST 1
#define AMATERASU_IDENTITY_PERMISSION_LAUNCH 2
#define AMATERASU_IDENTITY_PERMISSION_VIEW 3
#define AMATERASU_TLS_RSA_PKCS1_SHA256 0x0401
#define AMATERASU_TLS_RSA_PKCS1_SHA384 0x0501
#define AMATERASU_TLS_RSA_PKCS1_SHA512 0x0601
#define AMATERASU_TLS_RSA_PSS_RSAE_SHA256 0x0804
#define AMATERASU_TLS_RSA_PSS_RSAE_SHA384 0x0805
#define AMATERASU_TLS_RSA_PSS_RSAE_SHA512 0x0806

/* Methods may run concurrently while the binding remains live. Reply bytes are
 * borrowed only for the synchronous sink callback. No private key is exported.
 * Sign accepts a full TLS message, <=65536 bytes; PSS salt is hash-sized.
 * Verify checks current paired DER membership, not TLS proof of possession.
 * The TLS adapter must separately validate handshake signatures. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*certificate)(void *context, AmaterasuReplySink reply);
    AmaterasuStatus (*sign)(void *context, uint32_t scheme, AmaterasuBorrowedBytes message, AmaterasuReplySink reply);
    AmaterasuStatus (*verify_client)(void *context, AmaterasuBorrowedBytes certificate_der);
} AmaterasuIdentityApi;

/* Bind major 1, minor 1 and request this full size before using the appended
 * callback. Its unchanged prefix remains usable by version 1.0 consumers.
 * The callback checks current committed trust, validity and policy; list is
 * implied by view or launch, and view by launch. Only STATUS_OK grants access.
 * Unknown selectors return INVALID_ARGUMENT; unavailable state fails closed.
 * As with verify_client, TLS possession must be verified separately. */
typedef struct {
    AmaterasuIdentityApi v1_0;
    AmaterasuStatus (*check_permission)(void *context, AmaterasuBorrowedBytes certificate_der, uint32_t permission);
} AmaterasuIdentityApiV1_1;

#ifdef __cplusplus
}
#endif
#endif
