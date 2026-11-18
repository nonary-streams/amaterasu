# Publishing the RTX HDR add-on and legacy Linux set

The official Linux x86-64 add-on is installed from **Modules → Available → RTX
HDR → Add plugin**. It requires Core 0.1.1 and its matching host executable. The
installer checks and stages the NVIDIA runtime from NVIDIA's package index.
Once preparation finishes, select **Finish adding plugin** to activate it when
the host is idle. The Core package remains selected during the add-on trial.

Build Amaterasu's RTX HDR provider with `linux-nvidia` and build the stock
Display Settings and Foreground Context libraries from the same source. Then
run the following with the official Core signing key in `AMATERASU_PUBLISHER_KEY`.

```text
amaterasu-release pack-rtx-hdr-addon ARTIFACTS ADDON 0.1.0 0.1.1 SEQUENCE EXPIRY
amaterasu-release publish-repository BOOTSTRAP OUTPUT amaterasu LOCATION SEQUENCE EXPIRY \
  --versions CORE_VERSIONS --set amaterasu.runtime amaterasu-core CORE \
  --plugin amaterasu.rtx-hdr amaterasu-rtx-hdr ADDON
```

For this release, `CORE_VERSIONS` maps `amaterasu.runtime` to `0.1.1` and
keeps the source versions of the individual components. Keep the matching host
executable in the Linux release. The add-on has exact dependency versions and
ships only Amaterasu libraries. The host selects the SDK roles and supplies the
local runtime location. A host with RTX HDR bundled in Core already has those
components and needs a separate migration before installing the add-on.

## Legacy complete-set publishing

Older installations can use an explicitly selected, publisher-signed complete
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
