#ifndef AMATERASU_INPUT_H
#define AMATERASU_INPUT_H

/* Broker JSON contract, not a native function-table ABI. Use the host API to
 * send requests to this service. See docs/input-batch-contract.md. */
#define AMATERASU_INPUT_SERVICE_V1 "amaterasu.input/1"
#define AMATERASU_INPUT_MAX_BATCH_EVENTS 64u
#define AMATERASU_INPUT_MAX_LEASE_BYTES 128u
#define AMATERASU_INPUT_MIXED_BATCH_CAPABILITY "mixed_input_batches"

/* An active lease advertises mixed batches only with the boolean capability
 * set to true. On controller_unavailable, accepted is the completed prefix;
 * partial_event_possible marks the next event as potentially applied. The
 * caller must not replay accepted or uncertain input. */

#endif
