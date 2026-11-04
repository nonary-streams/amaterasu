namespace Amaterasu {
  const HOST_LOG_MAX_BYTES = 10 * 1024 * 1024;
  const HOST_LOG_CHUNK_BYTES = 4096;
  const HOST_LOG_PREVIEW_LINES = 300;
  const HOST_LOG_REFRESH_MS = 5000;
  const SUPPORT_BUNDLE_MAX_BYTES = 96 * 1024 * 1024;
  const SUPPORT_BUNDLE_CHUNK_BYTES = 8192;

  function hostLogText(source: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function"
      ? Amaterasu.localizedMessage(source)
      : source;
  }

  function hostLogSetLocalizedText(target: HTMLElement, source: string): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function") Amaterasu.setLocalizedTextContent(target, source);
    else target.textContent = source;
  }

  function hostLogReply(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The host returned an invalid log response.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true) throw new Error("The host log is unavailable. Refresh to try again.");
    return reply;
  }

  function hostLogInteger(value: unknown, maximum: number): number {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > maximum)
      throw new Error("The host returned an invalid log response.");
    return value;
  }

  export function renderHostLogs(context: PageContext, target: HTMLElement): () => Promise<void> {
    let generation = 0;
    let contents = "";
    let latestContents = "";
    let retainedBytes: Uint8Array[] = [];
    let retainedLength = 0;
    let retainedStream: string | null = null;
    let retainedDiscarded = 0;
    let pendingContents: string | null = null;
    let lines: string[] = [];
    let matches: number[] = [];
    let selected = 0;
    let paused = false;
    let disposed = false;
    let activeLoads = 0;
    let refreshPending = false;
    let timer: number | undefined;
    let observer: MutationObserver | undefined;
    let exportGeneration = 0;
    let exportBusy = false;
    let copyBusy = false;
    const following = el("span", "muted", "Following");
    following.setAttribute("role", "status");
    following.setAttribute("aria-live", "polite");
    const status = el("p", "muted");
    status.setAttribute("role", "status");
    const progress = el("p", "muted");
    progress.setAttribute("role", "status");
    const search = input("host_log_search", "", "search");
    search.placeholder = "Search the current host log";
    const level = el("select");
    level.name = "host_log_level";
    for (const value of ["All levels", "Verbose", "Debug", "Info", "Warning", "Error", "Fatal"]) {
      const option = el("option", "", value);
      option.value = value === "All levels" ? "" : value;
      level.append(option);
    }
    const output = el("pre", "host-log-output");
    output.setAttribute("aria-label", "Host log contents");
    const follow = input("host_log_follow", "", "checkbox");
    follow.checked = true;
    const followLabel = el("label", "host-log-follow", follow, el("span", "", "Follow newest"));
    const showLatest = button("Show latest", () => applyPending());
    showLatest.hidden = true;
    const pause = button("Pause", () => togglePause());
    const exportStatus = el("p", "muted");
    exportStatus.setAttribute("role", "status");
    const cancelExport = button("Cancel export", () => {
      if (exportBusy) { ++exportGeneration; exportStatus.textContent = "Cancelling support bundle…"; }
    });
    cancelExport.hidden = true;
    const exportButton = button("Download support bundle", () => void downloadSupportBundle());
    const previous = button("Previous match", () => move(-1));
    const next = button("Next match", () => move(1));
    const download = button("Download log", () => {
      const url = URL.createObjectURL(new Blob([latestContents], { type: "text/plain;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = "amaterasu-host-log.txt";
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    });
    const copyStatus = el("p", "muted");
    copyStatus.setAttribute("role", "status");
    copyStatus.setAttribute("aria-live", "polite");
    const copyVisible = button(hostLogText("Copy visible log view"), () => void copyVisibleLog());
    const refresh = button("Refresh host log", () => load());
    const controls = el(
      "div", "diagnostics-toolbar", field("Search host log", search), field("Level", level),
      followLabel, pause, refresh, copyVisible, download,
    );
    const matchControls = el("div", "diagnostics-match-navigation", previous, next, status);
    matchControls.setAttribute("role", "group");
    matchControls.setAttribute("aria-label", "Host log search navigation");
    const box = panel(
      "Host log",
      el("p", "muted", "Current host session only; older rotated segments may have been discarded."),
      controls,
      el("p", "muted", hostLogText("Copies only the log lines currently visible in this bounded view.")),
      copyStatus, following, showLatest, progress, matchControls, output,
      el("div", "support-log-export", exportButton, cancelExport, exportStatus,
        el("p", "muted", "The support bundle includes retained sessions from the current log location. Common identifying details are redacted; review it before sharing.")));
    target.append(box);

    function render(): void {
      const query = search.value.trim().toLocaleLowerCase();
      const filtered = lines.flatMap((line, index) => {
        const severity = /^\[[^\]]+\]: (Verbose|Debug|Info|Warning|Error|Fatal): /.exec(line)?.[1];
        return !level.value || severity === level.value ? [index] : [];
      });
      matches = query ? filtered.filter((index) => (lines[index] ?? "").toLocaleLowerCase().includes(query)) : [];
      if (selected >= matches.length) selected = 0;
      previous.disabled = next.disabled = !matches.length;
      download.disabled = !latestContents;
      if (query) {
        const index = matches[selected];
        status.textContent = index === undefined ? "No matching lines" : `Match ${selected + 1} of ${matches.length} · line ${index + 1}`;
        output.replaceChildren();
        if (index !== undefined) {
          const start = Math.max(0, index - 3);
          const end = Math.min(lines.length, index + 4);
          for (let line = start; line < end; line++) {
            if (line > start) output.append(document.createTextNode("\n"));
            const content = lines[line] ?? "";
            if (line === index) output.append(el("mark", "host-log-match", literalText(content)));
            else output.append(document.createTextNode(content));
          }
        }
      } else {
        status.textContent = filtered.length > HOST_LOG_PREVIEW_LINES
          ? `Showing the last ${HOST_LOG_PREVIEW_LINES} of ${filtered.length} matching lines. Search covers the full snapshot.`
          : `${filtered.length} matching lines in this snapshot.`;
        output.textContent = filtered.slice(-HOST_LOG_PREVIEW_LINES).map((index) => lines[index] ?? "").join("\n");
      }
      copyVisible.disabled = copyBusy || !output.textContent;
    }
    async function copyVisibleLog(): Promise<void> {
      if (copyBusy || disposed || !target.isConnected) return;
      const visibleText = output.textContent ?? "";
      if (!visibleText) return;
      copyBusy = true;
      copyVisible.disabled = true;
      try {
        if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable.");
        await navigator.clipboard.writeText(visibleText);
        if (!disposed && target.isConnected) hostLogSetLocalizedText(copyStatus, "Visible log view copied to the clipboard.");
      } catch {
        if (!disposed && target.isConnected)
          hostLogSetLocalizedText(copyStatus, "Could not copy the visible log view. Allow clipboard access and try again.");
      } finally {
        copyBusy = false;
        if (!disposed && target.isConnected) copyVisible.disabled = !output.textContent;
      }
    }
    function nearBottom(): boolean {
      const distance = output.scrollHeight - output.scrollTop - output.clientHeight;
      return !Number.isFinite(distance) || distance <= 24;
    }
    function viewerHasSelection(): boolean {
      const selection = window.getSelection?.();
      return !!selection && !selection.isCollapsed && !!selection.anchorNode && output.contains(selection.anchorNode);
    }
    function mayFollow(): boolean {
      return follow.checked && !search.value.trim() && nearBottom() && !viewerHasSelection();
    }
    function displayContents(value: string, scroll: boolean): void {
      contents = value;
      lines = contents ? contents.split("\n") : [];
      if (lines.at(-1) === "") lines.pop();
      render();
      if (scroll) output.scrollTop = output.scrollHeight;
    }
    function applyPending(): void {
      if (pendingContents === null) return;
      const pending = pendingContents;
      pendingContents = null;
      showLatest.hidden = true;
      selected = 0;
      displayContents(pending, follow.checked && !search.value.trim());
      updateFollowing();
    }
    function updateFollowing(): void {
      following.textContent = paused ? "Paused" : pendingContents === null ? "Following" : "Following · new log data available";
      pause.textContent = paused ? "Resume" : "Pause";
      pause.setAttribute("aria-pressed", String(paused));
    }
    function stopTimer(): void {
      if (timer !== undefined) window.clearTimeout(timer);
      timer = undefined;
    }
    function schedule(immediate = false): void {
      if (disposed || !target.isConnected || paused || document.visibilityState === "hidden" || activeLoads || timer !== undefined) return;
      timer = window.setTimeout(() => { timer = undefined; void load(); }, immediate ? 0 : HOST_LOG_REFRESH_MS);
    }
    function togglePause(): void {
      paused = !paused;
      updateFollowing();
      if (paused) {
        ++generation;
        refreshPending = false;
        stopTimer();
      } else if (activeLoads) refreshPending = true;
      else schedule(true);
    }
    function visibilityChanged(): void {
      if (document.visibilityState === "hidden") stopTimer();
      else if (!paused) {
        if (activeLoads) refreshPending = true;
        else schedule(true);
      }
    }
    function cleanup(): void {
      if (disposed) return;
      disposed = true;
      ++generation;
      ++exportGeneration;
      stopTimer();
      document.removeEventListener("visibilitychange", visibilityChanged);
      observer?.disconnect();
    }
    follow.addEventListener("change", () => { if (mayFollow()) applyPending(); });
    output.addEventListener("scroll", () => { if (mayFollow()) applyPending(); });
    function move(direction: number): void {
      if (!matches.length) return;
      selected = (selected + direction + matches.length) % matches.length;
      render();
    }
    search.addEventListener("input", () => { selected = 0; render(); });
    level.addEventListener("change", () => { selected = 0; render(); });
    search.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !event.isComposing && search.value.trim()) {
        event.preventDefault();
        move(event.shiftKey ? -1 : 1);
      }
    });

    async function downloadSupportBundle(): Promise<void> {
      if (exportBusy || disposed || !target.isConnected) return;
      exportBusy = true;
      const current = ++exportGeneration;
      const valid = () => !disposed && target.isConnected && current === exportGeneration;
      let snapshot: string | null = null;
      exportButton.disabled = true;
      cancelExport.hidden = false;
      exportStatus.textContent = "Preparing support bundle…";
      try {
        const opened = hostLogReply(await context.api.post<unknown>("host-logs/export_start"));
        if (typeof opened.snapshot !== "string" || !/^[a-f0-9]{32}$/.test(opened.snapshot) || opened.status !== "building")
          throw new Error("The host returned an invalid support bundle response.");
        snapshot = opened.snapshot;
        const deadline = Date.now() + 300_000;
        let ready: Record<string, unknown> | null = null;
        while (valid() && Date.now() < deadline) {
          const reply = hostLogReply(await context.api.post<unknown>("host-logs/export_status", { snapshot }));
          if (reply.snapshot !== snapshot || (reply.status !== "building" && reply.status !== "ready"))
            throw new Error("The host returned an invalid support bundle response.");
          if (reply.status === "ready") { ready = reply; break; }
          await new Promise<void>((resolve) => window.setTimeout(resolve, 400));
        }
        if (!valid()) return;
        if (!ready) throw new Error("The support bundle took too long to prepare.");
        const length = hostLogInteger(ready.length, SUPPORT_BUNDLE_MAX_BYTES);
        const sessions = hostLogInteger(ready.sessions, 32);
        if (!length || !sessions || typeof ready.truncated !== "boolean") throw new Error("The host returned an invalid support bundle response.");
        const chunks: ArrayBuffer[] = [];
        let offset = 0;
        while (offset < length && valid()) {
          const reply = hostLogReply(await context.api.post<unknown>("host-logs/export_read", { snapshot, offset, limit: SUPPORT_BUNDLE_CHUNK_BYTES }));
          const next = hostLogInteger(reply.next_offset, length);
          if (reply.snapshot !== snapshot || reply.offset !== offset || reply.length !== length || typeof reply.data !== "string" ||
              typeof reply.eof !== "boolean" || next <= offset || next - offset > SUPPORT_BUNDLE_CHUNK_BYTES || reply.eof !== (next === length))
            throw new Error("The host returned an invalid support bundle response.");
          let decoded: string;
          try { decoded = atob(reply.data); } catch { throw new Error("The host returned an invalid support bundle response."); }
          if (decoded.length !== next - offset) throw new Error("The host returned an invalid support bundle response.");
          chunks.push(Uint8Array.from(decoded, (character) => character.charCodeAt(0)).buffer);
          offset = next;
          if (valid()) exportStatus.textContent = `Downloading support bundle… ${Math.floor(offset * 100 / length)}%`;
        }
        if (!valid()) return;
        const url = URL.createObjectURL(new Blob(chunks, { type: "application/zip" }));
        const link = document.createElement("a");
        link.href = url;
        link.download = "amaterasu-support-logs.zip";
        link.click();
        window.setTimeout(() => URL.revokeObjectURL(url), 0);
        exportStatus.textContent = `${sessions} retained session${sessions === 1 ? "" : "s"} included.${ready.truncated ? " Some older data was omitted." : ""}`;
      } catch (error) {
        if (valid()) exportStatus.replaceChildren(errorPanel(error));
      } finally {
        if (snapshot) await context.api.post("host-logs/close", { snapshot }).catch(() => undefined);
        exportBusy = false;
        exportButton.disabled = false;
        cancelExport.hidden = true;
        if (target.isConnected && !disposed && current !== exportGeneration) exportStatus.textContent = "Support bundle cancelled.";
      }
    }

    async function load(): Promise<void> {
      if (disposed || !target.isConnected) return;
      stopTimer();
      if (activeLoads >= 2) { ++generation; refreshPending = true; return; }
      activeLoads++;
      const current = ++generation;
      const valid = () => !disposed && current === generation && target.isConnected;
      let snapshot: string | null = null;
      progress.textContent = "Opening host log…";
      try {
        const opened = hostLogReply(await context.api.post<unknown>("host-logs/open"));
        if (typeof opened.snapshot !== "string" || !/^[a-f0-9]{32}$/.test(opened.snapshot))
          throw new Error("The host returned an invalid log response.");
        snapshot = opened.snapshot;
        const length = hostLogInteger(opened.length, HOST_LOG_MAX_BYTES);
        const discarded = hostLogInteger(opened.earlier_segments_discarded, Number.MAX_SAFE_INTEGER);
        const stream = typeof opened.stream_id === "string" && /^[a-f0-9]{32}$/.test(opened.stream_id) ? opened.stream_id : null;
        const append = stream !== null && stream === retainedStream && discarded === retainedDiscarded && length >= retainedLength;
        const chunks = append ? [...retainedBytes] : [];
        let offset = append ? retainedLength : 0;
        while (offset < length && valid()) {
          const reply = hostLogReply(await context.api.post<unknown>("host-logs/read", { snapshot, offset, limit: HOST_LOG_CHUNK_BYTES }));
          const nextOffset = hostLogInteger(reply.next_offset, length);
          if (reply.snapshot !== snapshot || reply.offset !== offset || reply.length !== length ||
              typeof reply.data !== "string" || typeof reply.eof !== "boolean" ||
              nextOffset <= offset || nextOffset - offset > HOST_LOG_CHUNK_BYTES || reply.eof !== (nextOffset === length))
            throw new Error("The host returned an invalid log response.");
          let decoded: string;
          try { decoded = atob(reply.data); } catch { throw new Error("The host returned an invalid log response."); }
          if (decoded.length !== nextOffset - offset) throw new Error("The host returned an invalid log response.");
          const bytes = Uint8Array.from(decoded, (character) => character.charCodeAt(0));
          chunks.push(bytes);
          offset = nextOffset;
          if (valid()) progress.textContent = `Loading host log… ${Math.floor(offset * 100 / length)}%`;
        }
        if (!valid()) return;
        const changed = !append || offset !== retainedLength;
        retainedBytes = chunks;
        retainedLength = length;
        retainedStream = stream;
        retainedDiscarded = discarded;
        if (changed) {
          const decoder = new TextDecoder("utf-8");
          latestContents = chunks.map((bytes) => decoder.decode(bytes, { stream: true })).join("") + decoder.decode();
          if (!contents || mayFollow()) {
            pendingContents = null;
            showLatest.hidden = true;
            selected = 0;
            displayContents(latestContents, follow.checked && !search.value.trim());
          } else {
            pendingContents = latestContents;
            showLatest.hidden = false;
            download.disabled = !latestContents;
          }
          updateFollowing();
        }
        progress.textContent = discarded ? `${discarded} older log segments were discarded before this snapshot.` : "Snapshot loaded.";
      } catch (error) {
        if (valid()) {
          progress.replaceChildren(errorPanel(error));
        }
      } finally {
        if (snapshot) await context.api.post("host-logs/close", { snapshot }).catch(() => undefined);
        activeLoads--;
        if (!activeLoads) {
          const immediate = refreshPending;
          refreshPending = false;
          schedule(immediate);
        }
      }
    }
    document.addEventListener("visibilitychange", visibilityChanged);
    if (typeof MutationObserver !== "undefined") {
      observer = new MutationObserver(() => { if (!target.isConnected) cleanup(); });
      observer.observe(document.body, { childList: true, subtree: true });
    }
    render();
    void load();
    return load;
  }
}
