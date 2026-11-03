namespace Amaterasu {
  type RtxHdrPhase = "idle" | "downloading" | "verifying" | "installing" | "ready" | "failed" | "cancelled" | "activating" | "waiting_stream";
  export interface RtxHdrStatus {
    phase: RtxHdrPhase;
    transferred_bytes: number;
    total_bytes: number | null;
    error: string | null;
    restart_required: boolean;
    supported: boolean;
    runtime_ready: boolean;
    package_ready: boolean;
    provider_active: boolean | null;
  }

  export function parseRtxHdrStatus(value: unknown): RtxHdrStatus {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("RTX HDR status is invalid.");
    const status = value as Record<string, unknown>;
    const phases = ["idle", "downloading", "verifying", "installing", "ready", "failed", "cancelled", "activating", "waiting_stream"];
    if (status.ok !== true || typeof status.phase !== "string" || !phases.includes(status.phase) ||
        !Number.isSafeInteger(status.transferred_bytes) || (status.transferred_bytes as number) < 0 ||
        (status.total_bytes !== null && (!Number.isSafeInteger(status.total_bytes) || (status.total_bytes as number) < 0)) ||
        (status.error !== null && typeof status.error !== "string") ||
        typeof status.restart_required !== "boolean" || typeof status.supported !== "boolean" || typeof status.runtime_ready !== "boolean" ||
        typeof status.package_ready !== "boolean" ||
        (status.provider_active !== null && typeof status.provider_active !== "boolean")) throw new Error("RTX HDR status is invalid.");
    return status as unknown as RtxHdrStatus;
  }

  async function rtxHdrSettingsAvailable(context: PageContext): Promise<boolean> {
    let after: string | null = null;
    let revision: number | null = null;
    const seen = new Set<string>();
    let count = 0;
    do {
      const page: { items: { provider: string; options: { allowed_scopes: string[] }[] }[]; next: string | null; revision: number } =
        await context.api.post("session-extensions/list", { after, limit: 32, revision });
      if (!Array.isArray(page.items) || (page.next !== null && typeof page.next !== "string") ||
          !Number.isSafeInteger(page.revision) || page.revision < 0 || (revision !== null && page.revision !== revision))
        throw new Error("Invalid plugin settings discovery response.");
      revision = page.revision;
      count += page.items.length;
      if (count > 1024) throw new Error("Too many plugin settings providers.");
      if (page.items.some((descriptor) => descriptor.provider === "amaterasu.rtx-hdr" &&
          Array.isArray(descriptor.options) && descriptor.options.some((option) =>
            Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("host")))) return true;
      after = page.next;
      if (after && seen.has(after)) throw new Error("Plugin settings cursor repeated.");
      if (after) seen.add(after);
    } while (after);
    return false;
  }

  export function renderRtxHdrIntegration(context: PageContext, area: HTMLElement): void {
    let generation = 0;
    let timer: number | undefined;
    let settingsAvailable: Promise<boolean> | undefined;
    let awaitingRestart = false;
    const statusArea = el("div", "integration-status");
    const actions = el("div", "actions");
    const statusMessage = el("div");
    const card = el("section", "panel integration-card", el("h2", "", "RTX HDR"),
      el("p", "muted", "Convert supported SDR streams to HDR using NVIDIA RTX. The runtime is downloaded only when installed."),
      statusArea, actions, statusMessage);
    area.replaceChildren(card);

    function schedule(): void {
      if (timer !== undefined) window.clearTimeout(timer);
      timer = window.setTimeout(() => { if (card.isConnected) void refresh(); }, 1500);
    }
    async function refresh(): Promise<void> {
      const request = ++generation;
      try {
        if (awaitingRestart) settingsAvailable = undefined;
        const [reply, descriptorAvailable] = await Promise.all([
          context.api.post<unknown>("rtx-hdr/status", {}),
          settingsAvailable ??= rtxHdrSettingsAvailable(context).catch(() => false),
        ]);
        const status = parseRtxHdrStatus(reply);
        if (descriptorAvailable || status.phase === "failed") awaitingRestart = false;
        if (!card.isConnected || request !== generation) return;
        const activating = status.phase === "activating";
        const waiting = status.phase === "waiting_stream";
        const busy = ["downloading", "verifying", "installing"].includes(status.phase);
        const label = !status.supported ? "Unsupported" : (status.provider_active || descriptorAvailable) ? "Available" :
          status.phase === "failed" ? "Preparation failed" : status.phase === "cancelled" ? "Preparation cancelled" :
          status.phase === "downloading" ? "Downloading" : status.phase === "verifying" ? "Verifying" :
          status.phase === "installing" ? "Preparing plugin" : waiting ? "Waiting for stream to end" : activating ? "Activating" : status.package_ready ? "Plugin package staged" :
          status.runtime_ready ? "Runtime installed" : "Not installed";
        const detail = !status.supported ? "RTX HDR runtime installation is unavailable on this platform." :
          (status.provider_active || descriptorAvailable) ? "The RTX HDR provider is loaded. Configure it to enable conversion." : waiting ?
          "The current stream continues unchanged. Activation waits for the stream and game session to end. Cancel activation to resume a paused game." : activating ?
          "The host is checking the plugin and restarting the service before the next stream." : status.package_ready ?
          "The runtime and verified plugin package are staged. Activation waits for the current stream and game session to end." :
          status.runtime_ready ? "The runtime is installed. The plugin package has not been staged." :
          "Prepare the NVIDIA runtime and plugin package to make RTX HDR available.";
        const progress = status.phase === "downloading" ? (status.total_bytes && status.total_bytes > 0 ?
          `Downloaded ${Math.min(100, Math.floor(status.transferred_bytes * 100 / status.total_bytes))}%.` :
          `Downloaded ${status.transferred_bytes} bytes.`) : "";
        statusArea.replaceChildren(badge(label, status.phase === "failed" ? "bad" : ""),
          el("p", "muted", detail), ...(progress ? [el("p", "muted", progress)] : []),
          ...(status.error ? [el("p", "muted", literalText(status.error === "runtime_verification_failed"
            ? "The installed runtime failed verification and cannot be used. Repair requires an administrator."
            : status.error === "activation_failed" ? "The host could not activate the plugin. The previous package was retained; check the host logs and try again."
            : status.error === "runtime_install_failed" ? "The runtime could not be installed. Try again."
            : status.error === "rtx_hdr_package_unavailable" ? "The verified RTX HDR package could not be prepared. Check the approved repositories and connection, then try again." : status.error))] : []),
          ...(status.restart_required ? [el("p", "muted", "Restart required to apply the selected configuration.")] : []));
        const canWrite = context.api.allows("configuration.write");
        const action = button(waiting ? "Cancel activation" : busy ? "Cancel installation" : "Prepare RTX HDR", () => {
          void runAction(action, statusMessage, async () => {
            await context.api.post((busy || waiting) ? "rtx-hdr/cancel" : "rtx-hdr/install", {});
            if (waiting) awaitingRestart = false;
            await refresh();
          });
        });
        action.disabled = !status.supported || !canWrite || activating || status.phase === "verifying" || (!busy && !waiting && (status.package_ready || status.error === "runtime_verification_failed"));
        actions.replaceChildren(action);
        if (status.package_ready && !status.provider_active && !descriptorAvailable) {
          const activate = button("Activate RTX HDR", () => {
            void runAction(activate, statusMessage, async () => {
              awaitingRestart = true;
              try {
                await context.api.post("rtx-hdr/activate", {});
              } catch (error) {
                // A transport loss can happen after the host accepted the
                // request; reconcile status instead of blindly retrying.
                if (error instanceof ApiError) awaitingRestart = false;
                throw error;
              } finally {
                await refresh();
              }
            });
          });
          activate.disabled = !canWrite || busy || activating || waiting || awaitingRestart;
          actions.append(activate);
        }
        if (status.provider_active || descriptorAvailable) {
          const settings = button("Configure", () => showSessionExtensionSettings(context, { kind: "host" }, "RTX HDR settings", "amaterasu.rtx-hdr"));
          settings.disabled = !context.api.allows("configuration.read");
          actions.append(settings);
        }
        if (busy || waiting || activating || awaitingRestart) schedule();
      } catch (error) {
        if (card.isConnected && request === generation) {
          actions.replaceChildren();
          statusArea.replaceChildren(errorPanel(error, () => { void refresh(); }));
          if (awaitingRestart) schedule();
        }
      }
    }
    statusArea.replaceChildren(el("p", "muted", "Loading RTX HDR status…"));
    if (context.api.allows("configuration.read")) void refresh();
    else statusArea.replaceChildren(el("p", "muted", "This account cannot read RTX HDR status."));
  }
}
