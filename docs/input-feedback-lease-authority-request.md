# Feedback lease authority contract

The earlier proposal is superseded by pairing commit
`66cf199b9e0c4215e06e25b2aad58e1eb24106f5`. The current contract is documented
in [pairing effect authority](pairing-effect-authority.md), and the consumer
and remaining production integration are documented in
[input effect boundaries](input-effect-boundary-checkpoint.md).

`bind_stream` and `admit_stream_lease` use a structured scope containing a
controller slot, client-bound schema/version capabilities, and the selected
control transport service, stream and provider epoch. Admission and lookup
return the exact source and scope. The SDK checks the full returned objects and
performs an explicit checked conversion from general input capabilities to the
pairing-owned feedback capability. `complete_stream_lease` requires a matching
whole-control-stream retirement assertion; its fields alone do not authenticate
the transport receipt.

Production binding remains closed pending the authenticated protocol/lifecycle
join, owner status and retirement handshake, host-configuration fencing,
combined provider tests, restart recovery and independent review. The runtime
broker test uses the real pairing pipeline and a test-only source binding; its
retirement receipt is synthetic. The control-provider test separately covers a
real provider receipt and local ENet retirement.
