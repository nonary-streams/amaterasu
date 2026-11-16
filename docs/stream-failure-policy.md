# Stream failure and warning policy

A stream-start rejection must identify a condition that prevents a correct,
authorized stream or leaves an effect whose ownership cannot be established.
An unavailable optional feature, diagnostic, or advisory client hint must not
become an admission requirement. When a usable fallback exists, continue and
record what was unavailable and what was used instead.

Use a critical failure for authentication or authorization, invalid required
wire or encryption parameters, an unsupported selected media format, absence
of the required capture/encoder/transport, conflicting exclusive ownership, or
an unconfirmed native mutation that must be retired before reuse. Do not replace
a selected codec, chroma format, encryption mode, or explicit disabling policy
with a different one merely to produce a successful reply.

For an optional operation, the implementation and its tests should establish:

1. The fallback is safe and preserves the agreed media and security contract.
2. Rejection, an invalid reply, a missing service, and a lost or pending reply
   all reach the fallback without consuming the critical launch deadline.
3. A bounded warning includes a stable code and source, a reason, and useful
   requested/effective values or an OS error when available. Do not put raw
   payloads, paths, command text, or secrets in records.
4. A warning is logged independently of optional plugins. When a compatible
   history provider is present, attach it to the exact stream as a best-effort
   event. Failed diagnostic reporting must not itself reject the stream.
5. An explicit user setting and existing ownership/retirement fences still
   apply. A warning never asserts that an uncertain native effect has retired.

`amaterasu_module_sdk::stream_warnings` defines the shared scalar record,
validation and size bounds. The owner logs warnings and may query the launch
provider's optional `warnings` action after the launch reply is delivered. It
may submit `record_history_warnings` for the exact reserved RTSP activity.
Older or replacement providers can decline these actions without failing the
stream. The stream-session owner validates caller and activity identity,
deduplicates records, and forwards them as a `stream_warnings` history event
when the session activates. History uses its existing bounded best-effort
queue. Host logs are also bounded and subject to the runtime log service and
its retention policy; recording is not proof that a remote client received an
event.

## Audited paths in this change

| Path | Outcome |
| --- | --- |
| GameStream optional numeric hints and unsupported advisory quality, refresh, HEVC capability, and feature flags | Ignore or use the existing compatible setting; record a bounded stream adjustment. Required format, audio layout, transport, and encryption checks remain failures. |
| Moonlight launch metadata that is not used for media or input policy | Ignore malformed advisory values and record the field name and safe numeric value when available. HDR mode, audio selection, protocol version, and identity remain validated. |
| Initial or resumed Proton controller preference read | Use disabled or the last confirmed global value, respectively; client and application overrides still win. Bound the read and record the fallback. |
| Enabled Proton controller compatibility helper | If unavailable or unconfirmed, continue the application and record a warning. Retain an acquired helper reference until ordinary cleanup. |
| Configured application log | If unsupported, unopenable, or unattachable, continue with null output and report a warning. Invalid request syntax and unsafe nonregular output targets are still not used. |
| Best-effort detached command | Report executable lookup or spawn failures as warnings while the primary application continues. |
| Warning query and history submission | Run after the launch response and use bounded best-effort calls; absence, rejection, or uncertain delivery cannot invalidate the successful stream. |

## Remaining boundary

This is not yet an exhaustive classification of every stream failure. In
particular, NVHTTP can still retain a failed history-activity release as a
cleanup failure, including during pause or a rejected resume. That path needs
an independently retained, retryable history cleanup so diagnostic retirement
cannot stop a retained application or delay a later launch. It must preserve
the exact activity identity and must not declare native resources retired on
the strength of a diagnostic failure. The existing lifecycle, capture, display,
transport, and application retirement paths also require a case-by-case audit
before any further failure is demoted.
