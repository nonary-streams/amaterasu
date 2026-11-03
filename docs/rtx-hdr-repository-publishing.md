# Publishing the optional RTX HDR Linux session set

RTX HDR is distributed as an explicitly selected, publisher-signed complete
Linux session set. The normal installer and Linux session set do not contain the
RTX HDR provider or NVIDIA runtime. This workflow prepares repository files; it
does not upload, activate, or restart anything.

Build the ordinary Linux session artifacts and the RTX HDR provider with its
`linux-nvidia` feature. Initialize an RTX-enabled development bootstrap, then
sign the complete set with the normal release tool:

```text
amaterasu-release init-linux-session ROOT --rtx-hdr-runtime /absolute/path/to/runtime
amaterasu-release pack-linux-session-rtx-hdr ROOT ARTIFACTS PACKAGE SEQUENCE
amaterasu-release publish-rtx-hdr-repository BOOTSTRAP PACKAGE OUTPUT REPOSITORY_ID HTTPS_URL SIGNING_KEY_JSON CATALOG_SEQUENCE CATALOG_EXPIRY_UNIX
```

`BOOTSTRAP` is the JSON trust policy which verifies `PACKAGE`; the signing key
file is a JSON string containing a 32-byte Ed25519 secret as lowercase hex. Keep
the key outside the generated tree. The output directory must not already
exist. Select a catalog sequence greater than the last one used by that
repository; it must not be reused with different bytes. The expiry is a future
Unix timestamp at most one year away. The publisher verifies the source package
before signing the same composition with the requested release expiry.

The publisher verifies the source package, the complete RTX profile, and its
processor dependency and grants. It emits canonical signed `catalog.json` and
`catalog.sig`, plus the signed manifest and artifacts beneath
`native/amaterasu-linux-session-rtx-hdr/<version>/`. The catalog choice has
slot `amaterasu.rtx-hdr`, implementation
`amaterasu-linux-session-rtx-hdr`, and the version from the signed component.
It verifies the output using the repository consumer before returning. This is
a standalone one-choice catalog: do not replace a catalog containing other
choices without intentionally incorporating and signing those choices.

Repository verification uses the signing key fingerprint as the manifest key
ID. Publication canonicalizes and re-signs the manifest, which changes its
digest when the original was pretty-printed. Privileged helper approvals bind
to the final manifest digest and signer; review them for the published package.
The catalog's HTTPS location is a retrieval address, not a trust anchor. The
publisher key must be approved through the installed trust process.
