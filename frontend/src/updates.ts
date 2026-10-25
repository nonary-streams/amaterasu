namespace Amaterasu {
  export interface RepositoryChoice {
    slot: string;
    implementation: string;
    version: string;
    kind: "native_set" | "web_bundle";
    manifest_sha256: string;
  }

  export interface ApprovedRepositoryView {
    id: string;
    location: string;
    fingerprint: string;
    revoked: boolean;
    candidates: RepositoryChoice[];
  }

  export interface RepositoryStateSummary {
    approved: number;
    revoked: number;
    verifiedCandidates: number;
    uiCandidates: number;
    nativeSetCandidates: number;
    repositories: { id: string; status: string; candidates: number }[];
  }

  export interface PackageSelectionStatus {
    revision: string;
    active: string | null;
    previous: string | null;
    pending: string | null;
    trial: string | null;
    highestSequence: string;
    running: { available: boolean; digest: string | null };
  }

  export function parsePackageSelectionStatus(value: unknown): PackageSelectionStatus {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Package selection response is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true)
      throw new Error("Persisted package selection is unavailable.");
    if (typeof reply.revision !== "string" || !/^[a-f0-9]{64}$/.test(reply.revision))
      throw new Error("Package selection revision is invalid.");
    const digest = (key: string): string | null => {
      const candidate = reply[key];
      if (candidate === null) return null;
      if (typeof candidate !== "string" || !/^[a-f0-9]{64}$/.test(candidate))
        throw new Error("Package selection digest is invalid.");
      return candidate;
    };
    const highestSequence = reply.highest_sequence;
    if (typeof highestSequence !== "string" ||
        !/^(0|[1-9][0-9]{0,19})$/.test(highestSequence) ||
        (highestSequence.length === 20 && highestSequence > "18446744073709551615"))
      throw new Error("Package selection sequence is invalid.");
    const runningReply = reply.running;
    if (!runningReply || typeof runningReply !== "object" || Array.isArray(runningReply))
      throw new Error("Running package status is invalid.");
    const running = runningReply as Record<string, unknown>;
    if (running.available === true) {
      if (typeof running.digest !== "string" || !/^[a-f0-9]{64}$/.test(running.digest))
        throw new Error("Running package digest is invalid.");
    } else if (running.available !== false || running.digest !== undefined) {
      throw new Error("Running package status is invalid.");
    }
    return {
      revision: reply.revision,
      active: digest("active"),
      previous: digest("previous"),
      pending: digest("pending"),
      trial: digest("trial"),
      highestSequence,
      running: {
        available: running.available as boolean,
        digest: running.available === true ? running.digest as string : null,
      },
    };
  }

  export function summarizeRepositoryState(
    repositories: ApprovedRepositoryView[],
  ): RepositoryStateSummary {
    const approved = repositories.filter((repository) => !repository.revoked);
    const revoked = repositories.filter((repository) => repository.revoked);
    const candidates = approved.flatMap((repository) => repository.candidates);
    return {
      approved: approved.length,
      revoked: revoked.length,
      verifiedCandidates: candidates.length,
      uiCandidates: candidates.filter((candidate) => candidate.kind === "web_bundle").length,
      nativeSetCandidates: candidates.filter((candidate) => candidate.kind === "native_set").length,
      repositories: repositories.map((repository) => ({
        id: repository.id,
        status: repository.revoked
          ? "Revoked"
          : repository.candidates.length
            ? "Verified catalog choices available for this host"
            : "Approved; no verified choices currently available for this host",
        candidates: repository.revoked ? 0 : repository.candidates.length,
      })),
    };
  }

  export function parseRepositoryList(value: unknown): ApprovedRepositoryView[] {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Repository response is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Array.isArray(reply.repositories))
      throw new Error("Repository state is unavailable.");
    return reply.repositories.map((item: unknown) => {
      if (!item || typeof item !== "object" || Array.isArray(item))
        throw new Error("Repository entry is invalid.");
      const repository = item as Record<string, unknown>;
      if (typeof repository.id !== "string" ||
          typeof repository.location !== "string" ||
          typeof repository.fingerprint !== "string" ||
          typeof repository.revoked !== "boolean" ||
          !Array.isArray(repository.candidates))
        throw new Error("Repository entry is invalid.");
      const candidates = repository.candidates.map((raw: unknown) => {
        if (!raw || typeof raw !== "object" || Array.isArray(raw))
          throw new Error("Repository choice is invalid.");
        const choice = raw as Record<string, unknown>;
        if (typeof choice.slot !== "string" ||
            typeof choice.implementation !== "string" ||
            typeof choice.version !== "string" ||
            typeof choice.manifest_sha256 !== "string" ||
            !["native_set", "web_bundle"].includes(choice.kind as string))
          throw new Error("Repository choice is invalid.");
        return choice as unknown as RepositoryChoice;
      });
      return { ...repository, candidates } as ApprovedRepositoryView;
    });
  }

  export function parseRepositoryPage(value: unknown): {
    repositories: ApprovedRepositoryView[];
    nextOffset: number | null;
  } {
    const repositories = parseRepositoryList(value);
    const reply = value as Record<string, unknown>;
    const nextOffset = reply.next_offset;
    if (nextOffset !== null &&
        (!Number.isSafeInteger(nextOffset) || (nextOffset as number) <= 0 || (nextOffset as number) > 4096))
      throw new Error("Repository pagination is invalid.");
    return { repositories, nextOffset: nextOffset as number | null };
  }

  export function renderUpdates(context: PageContext): void {
    const area = el("div");
    const statusArea = el("div");
    const selectionArea = el("div");
    let loadRequest = 0;
    let selectionSnapshot: PackageSelectionStatus | null | undefined;
    let repositorySnapshot: ApprovedRepositoryView[] | null | undefined;
    context.content.append(
      sectionHeading("Modules & UI", "Choose verified implementations from approved publishers.", button("Refresh", load)),
      el("div", "notice",
        el("strong", "", "Publisher authority is broad"),
        el("p", "", "Approving a publisher allows it to supply any native module slot, including management, and the UI. Native modules share their worker's user privileges, while a replacement UI runs in the signed-in browser session and can use its API authority. Approval does not grant elevation. A signature proves source, not safe behavior."),
      ),
      panel("Persisted package selection", selectionArea,
        el("p", "muted", "Persisted phases are not a live runtime snapshot; the host reports the loaded package digest separately.")),
      panel("Read-only repository status", statusArea,
        el("p", "muted", "The authenticated API exposes repository listing only and has no approve, revoke, refresh, download, stage, install, activate, or restore actions. This page will not run shell commands.")),
      area,
    );

    async function load(): Promise<void> {
      const request = ++loadRequest;
      selectionSnapshot = undefined;
      repositorySnapshot = undefined;
      if (!context.api.allows("configuration.read")) {
        selectionSnapshot = null;
        repositorySnapshot = null;
        selectionArea.replaceChildren(empty(
          "Package selection unavailable",
          "This session needs configuration.read to inspect persisted package selection metadata.",
        ));
        statusArea.replaceChildren(empty(
          "Repository status unavailable",
          "This session needs configuration.read to inspect approved publishers and their verified catalog choices.",
        ));
        area.replaceChildren();
        return;
      }
      selectionArea.replaceChildren(el("p", "muted", "Loading persisted package selection…"));
      statusArea.replaceChildren(el("p", "muted", "Loading verified repository status…"));
      area.replaceChildren(el("p", "muted", "Loading verified repository choices…"));
      const selectionRequest = loadPackageSelection(request);
      void selectionRequest.then((selection) => {
        if (!selectionArea.isConnected || request !== loadRequest) return;
        selectionSnapshot = selection;
        if (selection) renderSelection(selection, repositorySnapshot);
      });
      try {
        const byId = new Map<string, ApprovedRepositoryView>();
        let offset = 0;
        while (true) {
          const page = parseRepositoryPage(await context.api.post<unknown>("repositories/list", { offset }));
          for (const repository of page.repositories) {
            const previous = byId.get(repository.id);
            if (previous && (previous.fingerprint !== repository.fingerprint || previous.revoked !== repository.revoked))
              throw new Error("Repository state changed during listing. Refresh to retry.");
            byId.set(repository.id, previous
              ? { ...previous, candidates: [...previous.candidates, ...repository.candidates] }
              : repository);
          }
          if (page.nextOffset === null) break;
          if (page.nextOffset <= offset) throw new Error("Repository pagination did not advance.");
          offset = page.nextOffset;
        }
        if (!area.isConnected || request !== loadRequest) return;
        const repositories = [...byId.values()];
        repositorySnapshot = repositories;
        if (selectionSnapshot) renderSelection(selectionSnapshot, repositories);
        renderStatus(repositories);
        renderChoices(repositories);
      } catch (error) {
        if (statusArea.isConnected && request === loadRequest) {
          repositorySnapshot = null;
          if (selectionSnapshot) renderSelection(selectionSnapshot, null);
          statusArea.replaceChildren(errorPanel(error, load));
          area.replaceChildren();
        }
      }
    }

    async function loadPackageSelection(request: number): Promise<PackageSelectionStatus | null> {
      try {
        return parsePackageSelectionStatus(
          await context.api.post<unknown>("package-selection/status", {}),
        );
      } catch (error) {
        if (selectionArea.isConnected && request === loadRequest) {
          selectionArea.replaceChildren(errorPanel(error, load));
        }
        return null;
      }
    }

    function renderSelection(
      selection: PackageSelectionStatus,
      repositories: ApprovedRepositoryView[] | null | undefined,
    ): void {
      const value = (digest: string | null) => digest ?? "None";
      const candidateMatch = (digest: string | null): string => {
        if (!digest) return "No package digest recorded";
        if (repositories === undefined) return "Loading current catalog match…";
        if (repositories === null) return "Current approved catalog unavailable";
        const matches = repositories.flatMap((repository) => repository.revoked
          ? []
          : repository.candidates
            .filter((candidate) => candidate.kind === "native_set" && candidate.manifest_sha256 === digest)
            .map((candidate) => `${repository.id}: ${candidate.implementation} ${candidate.version}`));
        return matches.length
          ? matches.join(" · ")
          : "Absent from the current approved catalog";
      };
      selectionArea.replaceChildren(
        table(
          ["Running worker package (host report)", "Package digest"],
          [[
            selection.running.available ? "Loaded in this worker" : "Digest unavailable",
            selection.running.digest ?? "Unavailable",
          ]],
        ),
        el("p", "muted", "The host supplies this digest from the verified package loaded in the worker. It does not establish individual module health."),
        table(
          ["Persisted phase", "Package digest", "Current approved catalog match"],
          [
            ["Active selection", value(selection.active), candidateMatch(selection.active)],
            ["Previous selection", value(selection.previous), candidateMatch(selection.previous)],
            ["Pending selection", value(selection.pending), candidateMatch(selection.pending)],
            ["Trial selection", value(selection.trial), candidateMatch(selection.trial)],
            ["Highest accepted sequence", selection.highestSequence, "—"],
          ],
        ),
        el("p", "muted", "Catalog matches require exact signed native manifest digest equality. They do not establish which publisher supplied the staged package or whether a trial passed."),
        ...(!selection.active ? [el("p", "muted", "No active package digest is recorded in selection metadata.")] : []),
      );
    }

    function renderStatus(repositories: ApprovedRepositoryView[]): void {
      const summary = summarizeRepositoryState(repositories);
      statusArea.replaceChildren(
        el("div", "section-meta",
          badge(`${summary.approved} approved`, summary.approved ? "good" : ""),
          badge(`${summary.revoked} revoked`),
          badge(`${summary.verifiedCandidates} verified choices`),
          el("span", "muted", `${summary.uiCandidates} UI bundles · ${summary.nativeSetCandidates} native sets for this host`),
        ),
        summary.repositories.length
          ? table(
              ["Publisher ID", "Read-only status", "Verified choices"],
              summary.repositories.map((repository) => [
                repository.id,
                repository.status,
                String(repository.candidates),
              ]),
            )
          : empty(
              "No approved repositories",
              "To add a publisher, use the explicit development release CLI after independently confirming its public-key fingerprint.",
            ),
      );
    }

    function renderChoices(repositories: ApprovedRepositoryView[]): void {
      const active = repositories.filter((repository) => !repository.revoked);
      const repositorySelect = el("select");
      const slotSelect = el("select");
      const implementationSelect = el("select");
      const details = el("div", "notice");
      const choices = panel("Choose an implementation",
        field("Approved repository", repositorySelect),
        field("Module slot or UI", slotSelect),
        field("Implementation and version", implementationSelect),
        details,
      );
      const addOption = (select: HTMLSelectElement, value: string, label: string) => {
        const option = el("option", "", label);
        option.value = value;
        select.append(option);
      };
      active.forEach((repository, index) =>
        addOption(repositorySelect, String(index), `${repository.id} · ${repository.fingerprint.slice(0, 16)}…`));
      const selectedRepository = () => active[Number(repositorySelect.value)];
      const selectedChoice = () => {
        const repository = selectedRepository();
        return repository?.candidates.filter((choice) => choice.slot === slotSelect.value)[Number(implementationSelect.value)];
      };
      const showChoice = () => {
        const repository = selectedRepository();
        const choice = selectedChoice();
        if (!repository || !choice) {
          details.replaceChildren(el("p", "muted", repository
            ? "No verified choice is available for this slot in the current host catalog. The repository may not have a current catalog, or its catalog may contain no choices for this host target."
            : active.length
              ? "Choose a publisher with a verified catalog candidate."
              : "No active approved publisher is available. Add or refresh one with the development release CLI."));
          return;
        }
        const command = choice.kind === "web_bundle"
          ? `amaterasu-release repository-install-ui ROOT ${repository.id} ${choice.implementation} ${choice.version} OUTPUT`
          : `amaterasu-release repository-download ROOT ${repository.id} ${choice.slot} ${choice.implementation} ${choice.version} OUTPUT`;
        details.replaceChildren(
          el("strong", "", `${choice.implementation} ${choice.version}`),
          el("p", "", `Source: ${repository.location}`),
          el("p", "", `Publisher key SHA-256: ${repository.fingerprint}`),
          el("p", "", `Signed manifest SHA-256: ${choice.manifest_sha256}`),
          el("p", "", choice.kind === "web_bundle"
            ? "Run this explicit administrator command to download, verify and select the UI. The previous UI remains recoverable at /recovery."
            : "This choice supplies a complete native component set. Download and stage it for review. A separate activate command explicitly authorizes its native code to run under the user account at the next development worker start or probe. Production native activation remains disabled."),
          el("code", "", command),
          ...(choice.kind === "native_set" ? [
            el("code", "", `amaterasu-release repository-stage-native ROOT ${repository.id} ${choice.slot} ${choice.implementation} ${choice.version} OUTPUT`),
            el("code", "", "amaterasu-release repository-activate-native ROOT STAGED_DIGEST"),
          ] : []),
        );
      };
      const fillImplementations = () => {
        implementationSelect.replaceChildren();
        selectedRepository()?.candidates
          .filter((choice) => choice.slot === slotSelect.value)
          .forEach((choice, index) => addOption(implementationSelect, String(index), `${choice.implementation} · ${choice.version}`));
        showChoice();
      };
      const fillSlots = () => {
        slotSelect.replaceChildren();
        const slots = [...new Set(selectedRepository()?.candidates.map((choice) => choice.slot) ?? [])];
        slots.forEach((slot) => addOption(slotSelect, slot, slot === "amaterasu.ui" ? "UI" : slot));
        fillImplementations();
      };
      repositorySelect.addEventListener("change", fillSlots);
      slotSelect.addEventListener("change", fillImplementations);
      implementationSelect.addEventListener("change", showChoice);
      fillSlots();
      if (!active.length) {
        repositorySelect.disabled = true;
        slotSelect.disabled = true;
        implementationSelect.disabled = true;
      }
      area.replaceChildren(
        panel("Approve a publisher",
          el("p", "", "On this development host, use repository-approve with the publisher's public key and a SHA-256 fingerprint confirmed through an independent channel. The repository URL alone cannot establish key identity."),
          el("code", "", "amaterasu-release repository-approve ROOT ID HTTPS_URL PUBLIC_KEY_HEX CONFIRMED_SHA256"),
          el("p", "", "Then run repository-refresh ROOT ID to retrieve the signed catalog. Key rotation uses a new publisher ID; revoke the old ID explicitly."),
        ),
        choices,
        panel("Recovery and selected-package inspection",
          el("p", "", "The management-owned /recovery page remains available when this UI bundle fails. For the host-owned read-only status and recovery path, use `amaterasu web-recovery-status ROOT`, then `amaterasu web-recover ROOT DIGEST` for a retained verified UI. `amaterasu native-recover ROOT [DIGEST]` restores a retained native set without loading it."),
          el("p", "muted", "The repository listing provides catalog choices. `POST /v1/package-selection/status` separately reports persisted native selection phases and the loaded worker package digest; both status endpoints require `configuration.read`. `web-recovery-status` inspects UI selection; `web-recover` and `native-recover` restore retained verified sets. Release floors stay intact.")),
      );
    }
    void load();
  }
}
