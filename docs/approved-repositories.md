# Approved publisher repositories

## Development workflow

Amaterasu's native release unit is a complete component set with one runtime and up to 64 feature libraries. The host verifies it before loading, stages a content-addressed copy, and trials it in a disposable worker at restart. UI assets are an independently signed web bundle selected without replacing the native set. The Windows service launches the worker in the active user's session. There is no general elevated broker yet.

The development administrator CLI supports `repository-approve`, `repository-revoke`, `repository-refresh`, `repository-list`, `repository-download`, `repository-install-ui`, `repository-stage-native`, `repository-activate-native`, and `repository-restore-ui`. Run `amaterasu-release` without arguments for exact syntax. These CLI mutations require a development bootstrap. The management console's **Plugins** page lists the modules the host loaded, the choices its repositories offer and the repositories themselves, and lets a signed-in administrator add, refresh and remove repositories on any host (see [Managing repositories from the console](#managing-repositories-from-the-console)). The management-owned `/recovery` page remains available independently of the selected UI. The fixed host shell also owns `web-recovery-status`, `web-recover`, and `native-recover ROOT [DIGEST]` commands. A specific retained, previously healthy digest can be recovered if the previous publisher was revoked; a merely staged set cannot be recovered directly to active. These recovery commands do not load selected native code, and `web-recover` and `native-recover` change only a development root. An installed Linux host returns to a retained set through its own authenticated restore instead, from the console or with `restore-package-offline` (see [Switching to an earlier set](#switching-to-an-earlier-set)). Native download and staging verify bytes without loading them. Only `repository-activate-native ROOT DIGEST` explicitly authorizes a future worker trial; a subsequent development `amaterasu probe ROOT` or worker startup executes that publisher's native code and promotes the set only if healthy.

Approval requires an ID, HTTPS location, Ed25519 public key, and its lowercase SHA-256 fingerprint confirmed through a separate trusted channel. The URL is only a byte location. Approval lets the publisher supply **any native component slot, including management or the runtime, and the UI**. This is broad publisher authority, not an add-on-only or cosmetic permission. A signature establishes provenance, not benign behavior or elevation. Native code shares the worker's user token and can use ordinary user process facilities; runtime grants are routing permissions, not a native code sandbox.

`catalog.json` is compact canonical JSON matching `repository::Catalog`'s field order. `catalog.sig` contains a lowercase hex Ed25519 signature over `amaterasu.repository.catalog.v1\n` followed by those exact JSON bytes. The catalog binds repository ID, OS/architecture target, expiry, increasing sequence, and each `(slot, implementation, version, kind, manifest_sha256)` choice. The signed manifest then binds each native artifact or UI asset hash and size. A candidate is accepted only under its approved publisher key. HTTPS retrieval refuses redirects and bounds headers and bodies. Repository files are at `catalog.json`, `catalog.sig`, and `{native|web}/{implementation}/{version}/manifest.{json|sig}` plus manifest-listed payload files. These paths are a transport convention, not a trust anchor.

Approval, revocation, the latest verified catalog, and its replay checkpoint are stored together under `repositories/approved.json` with an atomic replacement record. A repository ID cannot be reused after revocation; rotation currently requires explicit approval of a new ID and revocation of the old. UI selection keeps a signed release sequence/digest floor per publisher and for the project source. Development native selection also records the repository ID, pinned fingerprint and highest accepted native sequence/digest per publisher key in the atomic host selection record. A lower sequence or a different digest at an accepted sequence cannot be staged. Normal activation requires the current native floor and a still-listed, unexpired catalog choice; explicit recovery, and a restore on an installed Linux host, can select a retained older signed set without lowering the floor. The worker rechecks the live approval and signature under the approval lock through library loading. Revocation prevents subsequent starts, but does not stop a worker already running that code. A revoked selected UI becomes unavailable until restoration. The existing `stage_bundle` entry point relies on its caller to pass only project bootstrap keys; the repository staging entry point additionally binds each fingerprint to its key bytes.

## Platform and privilege model

The intended distribution is one logical signed catalog and package contract with prebuilt target-specific native libraries and portable web assets. Windows uses DLLs, Linux shared objects, and later macOS dylibs. End users should not need Cargo; building with Cargo is a publisher/developer workflow. The existing in-process native boundary is a versioned C ABI, not Rust's unstable ABI. Its interface registry permits namespaced services beyond a fixed product feature list. Component dependencies currently require exact versions and must be resolved as a complete set before activation. A dropdown choice does not imply a safe live native hot swap: the host activates a set at a session/restart boundary and trials it before promotion.

Publisher identity and privilege remain separate dimensions. Ordinary native components belong in a user-token worker on Windows, a non-root process on Linux, and a standard user process on macOS. The host loader now rejects an elevated Windows token or Unix effective root before loading ordinary libraries. This is a basic elevation check, not full token/capability confinement; it does not inspect individual Windows privileges or Linux capabilities. Native libraries share their worker's address space and user privileges.

An installed bootstrap policy can approve arbitrary namespaced API methods such as `org.example.paint/1` with exact provider, callers, interface version, request and response shapes, and a user/elevated privilege declaration. The user-process runtime accepts a descriptor only from its approved provider endpoint, negotiates versions, and validates both request and response. Shape checks cover top-level fields and coarse JSON kinds; handlers must still validate nested data, ranges and operation authority. Raw service calls cannot bypass a method contract. Delegated calls require approval for both the immediate caller and the original initiator. The host binds an endpoint to each verified component ID; the command payload cannot choose a caller ID. This is cooperative routing policy for native code, not isolation from hostile code in the same process. Core `amaterasu.*` and internal `hooks/` services cannot be shadowed by a dynamic approval. Elevated method registration is rejected by this broker. A future elevated process needs authenticated IPC, separately installed handlers and identity-bound grants; signing a module must not self-authorize privileged calls. The broker must not expose arbitrary commands or generic run-as-admin.

Windows virtual input will use a separately approved VHF-backed device path with
a minimal HID source driver and a session-bound companion, as detailed in
[Windows virtual input boundary](windows-vhf-input.md). The current input backend
still uses SendInput for keyboard/mouse. The separate UMDF VHF driver and
companion device transport now build but are not installed, selected, or
connected to an authenticated service/grant route.

## Remaining work and release blockers

- Development native sets can be staged with publisher-bound provenance and a durable per-publisher floor, then explicitly authorized for trial at a worker lifecycle boundary. This accepts a complete set from one publisher; independent mixing of components from multiple publishers remains unavailable. Production activation of a repository set exists only on an installed Linux host, as the host's own authenticated activation of a complete set from the publisher its protected policy names. The console reaches it to [update the running set](#updating-the-running-set-from-the-console) and to install RTX HDR. Revoking an active publisher requires stopping its running worker and manually recovering a retained set from a still-trusted signer before the next service start. A UI already loaded in a browser keeps running after revocation and may retain an API session; production revocation must also close affected browser sessions and invalidate their API credentials. New asset responses check approval under the repository lock. The development root's ordinary same-user files are not an adversarial filesystem boundary against replacement between hash verification and OS library loading; production needs protected ownership/ACLs or an equivalent immutable image-loading strategy.
- An installed Linux host can return to a retained set it ran healthy, from the console or, with the service stopped, with `restore-package-offline` (see [Switching to an earlier set](#switching-to-an-earlier-set)). `native-recover` and `web-recover` remain development CLI commands, and no other platform has a production recovery surface. A restore requires a retained signed set whose manifest has not expired and whose signer remains trusted; production needs a durable trusted recovery artifact or renewal plan for a host whose retained sets have all expired. The management `/recovery` page alone cannot serve that role.
- Composition of independently sourced components needs a whole-set dependency solver, interface/API negotiation, cross-platform platform requirements (including Linux libc), and health-tested activation. The present target string covers OS and architecture only.
- Installed user-process API registration, method schemas and grant checks are implemented. Browser access to those methods, grant review UI, privileged handler installation and authenticated elevated broker IPC are not. The browser HTTP router has a fixed route table.
- The console approves, refreshes and revokes a publisher under an administrator's session, on production hosts too. Production still needs machine-protected state/ACLs, key compromise response, revocation distribution, a refresh/freeze policy with reliable time, and closing the browser sessions of a revoked interface publisher. The development CLI is not a production trust manager.
- The console installs one thing a repository offers: a newer complete set from the repository that supplied the running one (see [Updating the running set from the console](#updating-the-running-set-from-the-console)). It can also return the host to a complete set it ran before (see [Switching to an earlier set](#switching-to-an-earlier-set)). It does not install an individual module, a set from another publisher, or another line of the same repository. It cannot approve grants: the host activates only the policy changes its protected allowlist names, so a set that needs any other change is refused at activation. A review of the grants a set's new components request, and compatibility diagnostics, are future work.

No real publisher key was enrolled, no downloaded third-party code was run, and no installed service permissions or persistent production trust were changed.

## Managing repositories from the console

The Plugins page reads `POST /v1/repositories/list` and `POST /v1/package-selection/status` with `configuration.read`. Three more routes change the approved repositories:

| Route | Body | Effect |
|---|---|---|
| `/v1/repositories/add` | `id`, `location`, `public_key`, `fingerprint` | Enrolls a publisher |
| `/v1/repositories/refresh` | `id` | Fetches and accepts the publisher's current catalog |
| `/v1/repositories/remove` | `id` | Revokes the publisher |

Each needs `configuration.write` and an administrator's own signed-in session. An API token is refused whatever its scopes, as it is for token and session management. The management module performs the change with its Network SDK provider on one worker thread; a second change is refused with `repository_busy` until the first finishes. The request returns an operation number at once, and the listing's `operation` field reports that change as `running`, `succeeded` or `failed` with a code until the next change starts.

Adding keeps the release tool's rule: the administrator supplies the public key **and** its fingerprint, and a fingerprint that does not belong to the key is refused before any network request. The console then fetches `catalog.json` and `catalog.sig` and records the approval only when that catalog verifies under the key for this host's target. A repository ID can never be enrolled twice, so a mistyped key or ID must not consume it. Enrolling lists a publisher's choices and adds its key to those that may sign a selected interface bundle. It does not download, stage or activate anything.

Removal is revocation and is permanent for the ID. It is refused with `repository_in_use` while the selection attributes the active, pending or trial native set to that repository, and with `repository_serves_console` while the selected interface bundle verifies only under that publisher's key. Either removal would stop the host or lock out its administrator at the next start.

The listing reports each repository's saved catalog as `current` (with its sequence and expiry), `expired`, `missing` or `unavailable`. A catalog that no longer verifies withholds that repository's choices only; the other repositories stay listed. `manageable` is false on a host whose management module has no network provider or web state, and absent on a host that predates these routes; the page then lists repositories without offering changes.

A publisher can hand out its details as one JSON object, which the Add dialog accepts when pasted into any field:

```json
{"id":"example-plugins","location":"https://plugins.example.org/amaterasu","public_key":"<64 hex characters>"}
```

The dialog fills in the ID, address and key from it. It never takes the fingerprint from pasted details: the administrator enters that separately, from a channel other than the repository address.

`POST /v1/modules/describe` with `{"id": "<component id>"}` returns one loaded component's declared dependencies, the components that require it, and the services it asked to provide and call, read from the running set's signed manifest. These are requests; host policy decides what is granted. Library file names are not returned.

## Updating the running set from the console

An installed Linux host can replace its whole running native set with a newer one from the repository that supplied it. The management module prepares the update and asks; the host shell decides. Before a trial the host re-reads the approval, the catalog and its protected policy allowlist and verifies every cached byte again. It waits for streams to end, and it rolls back a set that does not start.

Every route is `POST /v1/package-update/<action>` with an empty JSON object as its body. No request names a repository, a publisher, a package or a path: those come from the host's selection and startup composition.

| Action | Needs | Effect |
|---|---|---|
| `status` | `configuration.read` | Reports the state below |
| `check` | `configuration.write` | Refreshes the source repository's catalog and looks for a newer set |
| `download` | `configuration.write` | Places the offered set, verified, in the repository's candidate cache |
| `cancel` | `configuration.write` | Stops a check or download, or withdraws an activation the host has not started |
| `activate` | `configuration.write` and an administrator's own signed-in session | Asks the host to activate the downloaded set |

An API token is refused for `activate` whatever its scopes, as it is for repository changes. Every action that succeeds returns the status:

```json
{
  "ok": true,
  "supported": true,
  "phase": "available",
  "checked_unix_ms": 1794170000000,
  "source": { "repository": "amaterasu", "fingerprint": "<64 hex characters>" },
  "running": { "digest": "<64 hex characters>", "sequence": "1794169005" },
  "update": {
    "repository": "amaterasu", "slot": "amaterasu.runtime", "implementation": "amaterasu-linux-session",
    "version": "0.2.0", "digest": "<64 hex characters>", "sequence": "1794200000",
    "modules": [ { "id": "amaterasu.input", "from": "0.1.0", "to": "0.2.0", "change": "updated", "size": 3774568 } ],
    "download_bytes": 18200000, "total_bytes": 131000000
  },
  "transferred_bytes": 0,
  "error": null,
  "restoring": null
}
```

Sequences are decimal text so a browser keeps all 64 bits. `phase` is one of:

| Phase | Meaning |
|---|---|
| `idle` | Nothing has been checked since the module started |
| `checking` | A check is running |
| `current` | The repository offers nothing newer |
| `available` | `update` describes a newer set |
| `downloading` | The set is being fetched and verified |
| `ready` | The set is verified in the candidate cache |
| `waiting_stream` | The host holds the activation request until streams have ended |
| `activating` | The host is replacing the running set |
| `failed` | `error` says why |
| `cancelled` | A check or download was stopped |

`modules` lists every module of the offered set and every module only the running set has, ordered by identity. `change` is `updated` when the version differs, `rebuilt` when the version is the same and the library is not, and otherwise `added`, `removed` or `unchanged`. `from` is null for an added module; `to` is null and `size` is zero for a removed one. A reply lists at most 128 modules and stops earlier if unusually long names would exceed the response limit; the byte totals always cover the whole set. `total_bytes` is the size of every file the offered manifest signs, including privileged helpers. `download_bytes` leaves out the files the running set already holds, and `transferred_bytes` counts up to it.

### Which set is offered

The source is the repository the selection records as the supplier of the running set, under the publisher key recorded with it. A set with no recorded supplier has `error` `no_source` and nothing to check.

A check fetches the repository's catalog and accepts it under the same replay checkpoint as a refresh. Among the catalog's native sets, the host follows one line, a `(slot, implementation)` pair: the line that still lists the running set, or the only line offered when the running set is no longer listed. A catalog that singles out no line, including one that offers no native set, is `candidate_ambiguous`.

The check then fetches the signed manifest of each set on that line other than the running one, and no artifact. Each manifest must be the bytes the catalog names, carry the approved publisher's signature, and declare the slot at the version the catalog offers. A listed set whose manifest cannot be fetched or does not verify fails the check; it is not skipped, because a missing newest release would otherwise read as being up to date. The set with the highest release sequence is an update only when it is not the running set and its sequence is above both the running set's and the highest the host has accepted. The host refuses anything at or below that floor, so offering it could only fail.

### Download and activation

The download assembles the set beside the cache, verifies it as a whole against the signed catalog and manifest, and only then publishes it to `repositories/native-candidates/<repository>/<digest>` by a rename that never replaces an existing entry. A file the running set already holds with the same digest and size is copied from the running set instead of fetched. Where a byte came from does not matter to trust: the assembled set passes the same verification as one fetched entirely from the repository, and a held file that no longer matches is fetched instead. An entry that is already cached is verified again and kept.

`activate` sends the host the repository, the catalog digest, the set's digest, the active digest and the revision of the selection file. The host then does what it does for any activation: it waits for the stream barrier, stops the running modules, checks the request against the live approval, catalog, selection and allowlist, trials the set, and commits the policy, the selection and the service pin as one recoverable transaction. The service restarts into the new set, and a fresh status reads `idle` with the new running digest. If the host could not activate a set this repository's cache holds, the old set runs again and the status reads `failed` with `activation_failed` until the next check.

The host activates only when all of this is in place, none of which the console can provide:

- the Linux user service, which is the only host that accepts activation requests;
- installed policy that lets `amaterasu.management` call `amaterasu.host-package-activation/1`, which a default package requests and a default policy withholds;
- the private `package-activation-policy.json` in the host root, naming the publisher and the exact policy changes it may bring;
- the publisher's key among the bootstrap's trusted keys and its repository among the approved ones.

Without them `activate` reports `activation_unavailable` or, when the host refuses during the trial, `activation_failed`. The console cannot add a grant: a set whose components need a policy change the allowlist does not name is refused by the host.

`cancel` stops a running check or download; the phase changes to `cancelled` once the worker has returned, and nothing partial stays in the cache. While the host is still waiting for streams, `cancel` withdraws the request and the phase returns to `ready`. Once the host has started, `cancel` is refused. `restoring` is null throughout an update; it names the set of a [restore](#switching-to-an-earlier-set) the host holds or is performing.

Only one install runs at a time. While an RTX HDR install is preparing or activating a set, `check`, `download`, `activate` and `restore` are refused with `busy`, and while a package update or a restore is running or waiting, an RTX HDR install or activation is refused with `busy`.

### Errors and refusals

`error` in the status is one of:

| Code | Meaning |
|---|---|
| `unsupported_platform` | This host cannot activate a set |
| `no_source` | The selection records no repository as the running set's supplier |
| `repository_unavailable` | The source repository is unknown, revoked, held under another key, or its state could not be read |
| `catalog_download_failed` | The catalog or its signature could not be fetched |
| `catalog_rejected` | The catalog did not verify, or is older than one already accepted |
| `candidate_ambiguous` | The catalog singles out no line, or two sets claim the newest release |
| `manifest_download_failed` | A listed set's manifest or signature could not be fetched |
| `manifest_rejected` | A listed set's manifest is not the one the catalog names or did not verify |
| `download_failed` | A file of the set could not be fetched, or was longer than the manifest signs |
| `verification_failed` | The assembled or cached set did not verify |
| `activation_unavailable` | The host did not take the request, or the selection no longer names the running set |
| `activation_failed` | The host's last activation of a set from this repository, or its last return to a retained set, failed |
| `activation_interrupted` | The request the host held is gone and was never started |
| `shutting_down` | The module stopped the work because it is being unloaded |

A refused action changes nothing and returns `{"ok":false,"code":"..."}`:

| Code | Meaning |
|---|---|
| `busy` | A check, a download or another install is in progress |
| `nothing_to_download` | No check has found an update |
| `not_ready` | No downloaded set is waiting |
| `activation_in_progress` | The host holds or is performing an activation |
| `shutting_down` | The module is being unloaded |
| `unsupported_platform` | This host cannot activate a set |
| `not_restorable` | The digest a restore names is not a listed set the host would return to |
| `permission_denied` | The request did not come from a console route |

On a platform whose host has no activation, which today is every platform but Linux, `status` answers with `supported` false, phase `idle` and `error` `unsupported_platform`, and the other actions are refused with `unsupported_platform`. A Linux host that is not the installed user service reports `supported` true and fails `activate` with `activation_unavailable`.

### What this does not do

- It updates the whole set. Interfaces between components are not always versioned when they change, so one module is never replaced on its own.
- It follows the running set's own publisher and line. It does not install a module or a set from another repository, and it does not move a host between lines such as the session set with and without RTX HDR.
- It does not approve anything. It enrolls no key, adds no grant and changes no policy; a set that needs a policy change beyond the host's allowlist is refused by the host at activation.
- It does not offer again a release the host has already accepted. After a [return to an earlier set](#switching-to-an-earlier-set), the releases between that set and the newest accepted one are reached from the list of retained sets, not from a check.
- It does not update the host executable, the installer or the web interface.
- It does not check on its own schedule. A check runs when the console asks.
- It reads the catalog from the repository's address every time. After a [local import](local-package-updates.md) whose catalog is ahead of the published one, a check fails with `catalog_rejected` until the repository publishes a catalog at or above it.

## Switching to an earlier set

An installed Linux host keeps every set it ran healthy and can make one of them the running set again. This is a change of the active set and nothing else: the accepted release does not move down. `highest_sequence` and the publisher floors in the selection stay where they were, so a later update must still be newer than every release the host has accepted, and a repository cannot use a host's older running set to offer it an old release again.

### Listing the retained sets

`POST /v1/package-update/versions` with an empty JSON object and `configuration.read` returns the newest retained sets:

```json
{
  "ok": true,
  "supported": true,
  "total": 156,
  "versions": [
    { "digest": "<64 hex characters>", "sequence": "1794169006", "state": "running",
      "installed_unix_ms": 1794529635000, "modules": 37, "differs": 0,
      "restorable": false, "reason": null, "switches_back": true },
    { "digest": "<64 hex characters>", "sequence": "1794169005", "state": "previous",
      "installed_unix_ms": 1794443012000, "modules": 37, "differs": 2,
      "restorable": true, "reason": null, "switches_back": false }
  ]
}
```

`versions` holds at most 24 sets, newest release first, and always includes the running one; `total` counts every healthy set whose files the host still has. `state` is `running`, `previous` for the set the selection records as the one before it, and otherwise `retained`. `installed_unix_ms` is when the host stored the set's manifest, or null. `differs` counts the modules whose library is not the running set's, including modules only one of the two sets has. `reason` says why a set that is not running cannot be returned to:

| Reason | Meaning |
|---|---|
| `expired` | The set's signed manifest has expired |
| `publisher_untrusted` | The set was accepted under another key than the running set's, or its repository is no longer approved and trusted |
| `unavailable` | The selection has no record of who published the set, or the set is not at or below its publisher's accepted release |

`switches_back` says whether the set's own console could switch sets again after a return to it:

| `switches_back` | Meaning |
|---|---|
| `true` | The set has run on this host with a management module that serves `versions` and `restore` |
| `false` | It is not known to have: it last ran before these routes existed, it has never run on a host that takes restores, or the record of it is gone |

This is a fact about what has run, not a reading of the set. Nothing a publisher signs says whether a set's management module serves these routes, so a set that does serve them records its own digest once the host shell has shown that it takes restore requests, which happens within the first seconds after it starts. The running set therefore lists `true` once it has recorded itself, and a retained set lists `true` only if it did so while it ran. The record is `switching-sets.json` in the host's `repositories` directory, where the module already keeps the approved repositories and the candidate cache. It names at most the 256 sets that ran most recently, and it is read and written on the worker thread that reads the manifests.

`switches_back` changes nothing the host accepts. The host never reads the record, `restorable` does not depend on it, and a set that lists `false` is returned to in exactly the same way as one that lists `true`. The console uses it to warn before the switch, because the way forward from such a set is [the command line](#with-the-service-stopped). A record that is missing, cannot be read or is not a record names no set: every set then lists `false` and the listing is otherwise unaffected. A file that is not a record is replaced the next time a set records itself; something that cannot be read as a file is left as it is.

The module reads one manifest for each retained set on a worker thread when it starts, and again when a request finds the list more than 30 seconds old. A request is answered from the last read. A request that arrives before the first read has finished gets an empty list with `"loading": true`. The list is advice: the host decides when a restore is asked for.

A host that cannot return to a set answers `{"ok":true,"supported":false,"total":0,"versions":[]}`. That is every platform but Linux, a Linux host that is not the installed user service, a host whose policy withholds `amaterasu.host-package-activation/1` from management, and a host executable from before this feature that runs a newer module set.

### Returning to one

`POST /v1/package-update/restore` with `{"digest":"<64 hex characters>"}` needs `configuration.write` and an administrator's own signed-in session, as `activate` does; an API token is refused. The request names a digest and nothing else. The module answers with the [status](#updating-the-running-set-from-the-console) in phase `waiting_stream` and `restoring` set to the digest and release sequence of the set, or refuses with `busy`, `activation_in_progress`, `shutting_down`, `unsupported_platform` or `not_restorable`. A digest is `not_restorable` when the list does not show it as restorable: an unknown digest, the running set, an expired or untrusted one, or a retained set too old to be among the 24 listed.

From there a restore follows an update's phases. `cancel` withdraws it while the host waits for streams to end, and the status returns to the phase it had before the restore was asked for. Once streams have ended the phase is `activating` and `cancel` is refused. The service restarts into the restored set, and a fresh status reads `idle` with that set running. If the set did not pass its trial, or the restarted service did not start, the previous set runs again and the status reads `failed` with `activation_failed` until the next check.

### What the host verifies

Management asks through `amaterasu.host-package-activation/1`, the grant it already needs for an update, so a policy that lets it update lets it restore and no policy change is involved. The request carries the digest, the active digest and the revision of the selection file. The host shell then, holding its root lock and with the running modules stopped:

- reads its protected `package-activation-policy.json` and requires a signed production bootstrap that grants management the activation service;
- requires the selection to be unchanged, with no pending or trial set;
- requires the digest to be a set the selection records as healthy, with its supplier recorded, and not the running set;
- requires that supplier's key to be the publisher the allowlist names, and its repository approval to be live and trusted by the bootstrap, and holds that approval locked until the transaction has committed;
- derives the bootstrap policy from the allowlist as an activation does, so a set that needs another policy is refused;
- verifies every file of the retained set again under that publisher's key alone, refuses an expired manifest, and requires the set to be at or below its publisher's accepted release without being a different set at that release's own number;
- loads the set once, checks its health and retires it;
- verifies the files once more and commits the policy, the selection, the input companion's package binding and the service pin as one journaled transaction.

The selection changes in two fields: `active` becomes the restored set and `previous` the set that was running. A transaction interrupted at any point is rolled back at the next start, and the journal authorizes nothing by itself: recovery rebuilds the expected transition from the allowlist and the signed sets and accepts either an update, which raises the accepted release, or a restore, which leaves every record but `active` and `previous` as it was.

At every start the host accepts as its active set the release at its publisher's floor, or an older set that is healthy, recorded with its supplier, signed by that live and trusted publisher, and unexpired. The readiness probe and the release tool's preflight and policy review use the same rule.

### With the service stopped

`amaterasu restore-package-offline ROOT MANIFEST_DIGEST ACTIVE_DIGEST SELECTION_REVISION` performs the same restore for a stopped managed service, under the checks `activate-package-offline` makes of the unit, the host executable and the root. `SELECTION_REVISION` is the SHA-256 of `selection.json`. Starting the service then completes the trial.

This is also the way forward from a set that predates this feature, which the list shows with `switches_back` false. Its management module has no `versions` or `restore` route, so its console cannot switch sets, and an update check does not offer a release the host has already accepted. The newer sets are still retained; `restore-package-offline` returns to one.

### Compatibility between host and set

The runtime is part of the set and the host shell is not, so either can be older than the other.

- A host with this feature runs an older set unchanged. It asks the runtime for restore requests at most every 100 milliseconds; a runtime from before them answers `invalid_request` to the unknown command, and the host stops asking that runtime.
- A set with this feature runs on an older host unchanged. The runtime admits a restore only after the host shell has asked for one, so on a host that never asks the request is refused at once instead of holding the stream barrier closed, and the module reports `supported: false`.
- The request and the commands are additions to the runtime's JSON control messages. No existing message, the native ABI, the runtime configuration or the selection file changed shape, so a retained set's own modules read the selection a restore wrote.
- A host executable from before this feature does not start a selection whose active set is below the accepted release. Return to the newest accepted set before replacing the host executable with an older one.

### What this does not do

- It does not remove a retained set, or free the space retained sets take.
- It does not return to a set this host never ran, or to one whose files, signature or publisher approval no longer hold.
- It does not restore module state. A set reads whatever its modules' newer versions have since saved; a set that cannot start with it fails its trial or its first start, and the host returns to the set it was running.
- It does not change the host executable or the web interface.

## Including an independently built library in a local set

The development `pack-linux-session` command accepts an explicit
`--external-component TEMPLATE_JSON ARTIFACT` pair after its sequence number.
The publisher builds the external library separately, then supplies its reviewed
component manifest as `TEMPLATE_JSON` and the library bytes as `ARTIFACT`. The
manifest uses the ordinary `ComponentManifest` fields, including the identity,
version, output filename, ABI major, exact component dependencies, and requested
routing grants. Its `sha256` and `size` fields are placeholders: packaging
recomputes both from the selected artifact. The external component must not be
a runtime, and its identity and output filename must differ from the profile's
components.

Before packaging, the development root's bootstrap policy must separately
approve every requested grant for that exact component identity. Provider API
methods, lifecycle hooks, descriptor hashes, caller scopes, and component
settings also need their own explicit policy approvals where applicable; the
artifact or template cannot authorize them. The publisher signs the *entire*
component set with the local development key and verifies the resulting
signature, target, artifact hashes, identities, and dependency graph. Package
selection does not stage, activate, or load a library. The selected native code
is trusted in-process code, and broker grants are not a native-code sandbox.
This workflow does not mix independently signed sets or establish production
publisher trust.

When a selected library supplies a service that an existing component must call,
`--call-grants CALLS_JSON` may follow the external-component pair. The JSON is
an object mapping selected component IDs to arrays of service names, for example
`{"amaterasu.extension-registry":["example.display/1"]}`. Each service must
be provided by a component selected in the signed set and independently approved in
the installed bootstrap policy for each named caller. The publisher adds only
those calls to the signed component requests; an absent, unselected, or
unapproved caller or service is rejected. Add every required transit caller
explicitly. API method schemas and caller authorization still require their own
operator policy approval. For a worked
application-library provider binding and lifecycle limitations, see
[provider configuration](application-library-provider-configuration.md).
