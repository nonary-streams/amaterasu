# Local signed package updates

A local installation does not require uploading a release. Use the existing
publisher key to prepare a signed native package and signed repository layout
on disk. The bundle contains `catalog.json`, `catalog.sig` and
`native/IMPLEMENTATION/VERSION/` with the complete package. Use a catalog
sequence above the installed repository checkpoint and a package sequence above
the installed publisher floor. Local imports advance those same replay floors;
future shared releases must also advance them.

Run the release tool built from the compatible source revision:

```text
amaterasu-release import-local-native ROOT REPOSITORY BUNDLE MANIFEST_DIGEST
```

This verifies the existing approval, bootstrap trust, catalog signature, target,
expiry, manifest and every artifact before accepting the catalog. It caches the
package without loading code or changing selection. Corrupt downloads can be
retried. It never enrolls a publisher or contacts the repository URL.

Review the exact package and policy delta, snapshot recovery state, check that
no stream is active, and stop the managed service. Use the installed host so its
executable hash and service pin continue to match:

```text
amaterasu activate-package-offline ROOT REPOSITORY CATALOG_DIGEST MANIFEST_DIGEST ACTIVE_DIGEST SELECTION_REVISION
```

`SELECTION_REVISION` is the SHA-256 of the current selection file. The existing
host activation policy must authorize the exact destination policy. Start the
managed service to complete the trial, then verify the selected and loaded
digest, readiness, cleanup journals and local HTTP service. Use
`reconcile-package-activation` if authenticated candidate recovery is needed.
Never edit cleanup evidence or selection files to manufacture success.

Install complete sets only. Interfaces between components are not always
versioned when they change, so replacing some libraries can produce a set that
loads and then fails while streaming. `scripts/local_full_update.py prepare`
refuses any library or host not compiled from the named tree and records what it
installed; `scripts/local_full_update.py check ROOT` reports later drift.
Both preparation and the installed audit reject an enabled Linux virtual-display
provider without its compiled `linux-libvirtualdisplay` backend. A library built
from the right source tree alone does not establish that the backend is present.

For an existing default Linux installation that used exclusive external-capture
input, `prepare --bound-input` includes the exact input/coordinator grants in the
candidate manifest and emits a proposed protected activation allowlist. It
preserves prior approved destinations and rejects independent input providers
or unapplied earlier approvals. Preparation changes no installed policy;
activation snapshots the original state and installs the reviewed allowlist
only after checking activity and stopping the service. `--target-dir PATH`
supports build output on a separate volume while retaining source checks.

An install request authorizes these routine local steps and an idle service
restart. Interrupting an active stream and publishing a shared release are
separate actions. Report real client playback only after it is observed.
