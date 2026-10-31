namespace Amaterasu {
  interface ManagedLaunch {
    reference: string;
    revision: string;
    state: "starting" | "ready" | "stopping" | "ended" | "cleanup_failed";
    pending_resume: boolean;
  }

  export function parseManagedLaunchStatus(value: unknown): ManagedLaunch | null {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Managed stream status is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true) throw new Error("Managed stream status is unavailable.");
    if (reply.launch === null) return null;
    if (!reply.launch || typeof reply.launch !== "object" || Array.isArray(reply.launch)) throw new Error("Managed stream status is invalid.");
    const launch = reply.launch as Record<string, unknown>;
    if (typeof launch.reference !== "string" || !/^[a-f0-9]{64}$/.test(launch.reference) ||
        typeof launch.revision !== "string" || !/^[a-f0-9]{64}$/.test(launch.revision) ||
        typeof launch.state !== "string" || !["starting", "ready", "stopping", "ended", "cleanup_failed"].includes(launch.state) ||
        typeof launch.pending_resume !== "boolean") throw new Error("Managed stream status is invalid.");
    return launch as unknown as ManagedLaunch;
  }

  export function renderManagedStreamControls(context: PageContext, container: HTMLElement): void {
    const canStop = (): boolean => typeof context.api.allowsLaunchAction === "function" && context.api.allowsLaunchAction("stop");
    if (!context.api.allows("session.read") || !canStop()) return;
    let disposed = false;
    let generation = 0;
    let timer: number | undefined;
    let current: ManagedLaunch | null = null;
    let watched: string | null = null;
    let busy = false;
    let notice = "";
    const description = el("p", "muted");
    const message = el("p", "muted");
    const stop = button("Stop managed stream", () => void requestStop(), "secondary");
    const box = panel("Managed stream controls", description, message, stop);
    box.hidden = true;
    container.append(box);

    function render(): void {
      box.hidden = !current && !notice;
      if (!current) {
        description.textContent = "";
        message.textContent = notice;
        stop.hidden = true;
        return;
      }
      const state = current.state;
      description.textContent = state === "cleanup_failed"
        ? "The host could not confirm all stream and application cleanup. You can retry cleanup."
        : state === "stopping"
          ? "Stopping the managed stream and its application; cleanup is still in progress."
          : state === "ended"
            ? "The managed launch has ended and its cleanup completed."
            : "Stopping the managed stream also closes its associated application and may discard unsaved work.";
      message.textContent = notice || (current.pending_resume ? "A pending reconnect will also be cancelled." : "");
      stop.hidden = state === "stopping" || state === "ended";
      stop.disabled = busy || !canStop();
      stop.textContent = state === "cleanup_failed" ? "Retry stream cleanup" : "Stop managed stream";
    }

    function cleanup(): void {
      if (disposed) return;
      disposed = true;
      generation++;
      if (timer !== undefined) window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", visibilityChanged);
      observer.disconnect();
    }
    function visibilityChanged(): void {
      if (!container.isConnected) { cleanup(); return; }
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
      generation++;
      if (document.visibilityState === "visible") void refresh();
    }
    async function refresh(): Promise<void> {
      if (disposed || !container.isConnected || document.visibilityState !== "visible") return;
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
      const request = ++generation;
      const reference = watched;
      try {
        const launch = parseManagedLaunchStatus(await context.api.post<unknown>("nvhttp/launch_status", reference ? { reference } : {}));
        if (disposed || !container.isConnected || request !== generation) return;
        if (reference && (!launch || launch.reference !== reference)) throw new Error("Managed stream identity changed.");
        current = launch;
        if (reference && launch?.state === "ended") {
          notice = "The host confirmed stream and application cleanup.";
          watched = null;
        } else if (reference && launch?.state === "cleanup_failed") {
          notice = "Cleanup is not confirmed. Review the host status before retrying.";
          watched = null;
        }
        render();
      } catch {
        if (disposed || !container.isConnected || request !== generation) return;
        current = null;
        notice = reference ? "Stop outcome is unconfirmed. Refresh to check the host before retrying." : "Managed stream controls are unavailable.";
        render();
      } finally {
        if (!disposed && container.isConnected && request === generation && document.visibilityState === "visible")
          timer = window.setTimeout(() => void refresh(), 3000);
      }
    }
    async function requestStop(): Promise<void> {
      const selected = current;
      if (!selected || busy || !canStop() || selected.state === "stopping" || selected.state === "ended") return;
      busy = true;
      render();
      const retry = selected.state === "cleanup_failed";
      const approved = await confirmAction(retry ? "Retry stream cleanup?" : "Stop the managed stream?",
        retry ? "The host will retry cleanup of the same managed launch." :
          "This ends the current managed stream and its associated application. Unsaved work may be lost. A pending reconnect will be cancelled.",
        retry ? "Retry cleanup" : "Stop stream");
      if (!approved || disposed || !container.isConnected || !canStop()) {
        busy = false;
        render();
        return;
      }
      generation++;
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
      watched = selected.reference;
      current = null;
      notice = "Requesting stream cleanup…";
      render();
      try {
        const result = await context.api.post<unknown>("nvhttp/stop_launch", { reference:selected.reference, revision:selected.revision });
        if (!result || typeof result !== "object" || (result as Record<string, unknown>).ok !== true ||
            (result as Record<string, unknown>).accepted !== true || (result as Record<string, unknown>).reference !== selected.reference)
          throw new Error("Stop admission was not confirmed.");
        notice = "Stop requested. Waiting for the host to confirm cleanup.";
      } catch (error) {
        if (error && typeof error === "object" && (error as {code?: unknown}).code === "launch_changed") {
          watched = null;
          notice = "The launch changed while confirmation was open. Review the current stream before trying again.";
        } else notice = "Stop outcome is unconfirmed. Checking the host; do not repeat the request yet.";
      } finally {
        busy = false;
        if (!disposed && container.isConnected) { render(); void refresh(); }
      }
    }
    const observer = new MutationObserver(() => { if (!container.isConnected) cleanup(); });
    observer.observe(document.body, { childList:true, subtree:true });
    document.addEventListener("visibilitychange", visibilityChanged);
    void refresh();
  }
}
