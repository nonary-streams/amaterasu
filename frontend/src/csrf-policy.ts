namespace Amaterasu {
  export interface CsrfPolicyReply {
    ok: true;
    revision: number;
    values: { csrf_allowed_origins: string[] | null };
    csrf_allowed_origins: string[];
  }

  export function validCsrfOrigin(origin: unknown): origin is string {
    if (typeof origin !== "string" || origin.length > 256 || !origin.startsWith("https://")) return false;
    const authority = origin.slice(8);
    let host: string;
    let port: string | undefined;
    if (authority.startsWith("[")) {
      const match = /^\[([0-9a-fA-F:.]+)\](?::([0-9]+))?$/.exec(authority);
      if (!match) return false;
      try { new URL(origin); } catch { return false; }
      port = match[2];
    } else {
      const parts = authority.split(":");
      if (parts.length > 2) return false;
      host = parts[0] ?? "";
      port = parts[1];
      if (/^[0-9.]+$/.test(host)) {
        const octets = host.split(".");
        if (octets.length !== 4 || octets.some(value => !/^(0|[1-9][0-9]{0,2})$/.test(value) || Number(value) > 255)) return false;
      } else if (host.length === 0 || host.length > 253 || /^(?:[0-9]+|0[xX][0-9a-fA-F]*)$/.test(host.split(".").at(-1) ?? "") || host.split(".").some(label =>
        label.length === 0 || label.length > 63 || !/^[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?$/.test(label))) return false;
    }
    return port === undefined || (/^[0-9]+$/.test(port) && Number(port) > 0 && Number(port) <= 65535);
  }

  function csrfOrigins(value: unknown): value is string[] {
    return Array.isArray(value) && value.length <= 16 && value.every(validCsrfOrigin) && new Set(value).size === value.length;
  }

  function sameCsrfOrigins(left: string[] | null, right: string[] | null): boolean {
    return left === null || right === null ? left === right :
      left.length === right.length && left.every((origin, index) => origin === right[index]);
  }

  export function parseCsrfPolicyReply(value: unknown): CsrfPolicyReply {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("CSRF origin policy reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("CSRF origin policy reply is invalid.");
    const selection = (reply.values as Record<string, unknown>).csrf_allowed_origins;
    if ((selection !== null && !csrfOrigins(selection)) || !csrfOrigins(reply.csrf_allowed_origins) ||
        !sameCsrfOrigins(selection ?? [], reply.csrf_allowed_origins))
      throw new Error("CSRF origin policy was not confirmed by the host.");
    return { ok: true, revision: reply.revision as number,
      values: { csrf_allowed_origins: selection === null ? null : [...selection] },
      csrf_allowed_origins: [...reply.csrf_allowed_origins] };
  }

  export async function saveCsrfPolicy(
    api: Pick<Api, "post">,
    current: CsrfPolicyReply,
    selection: string[] | null,
  ): Promise<CsrfPolicyReply> {
    if (selection !== null && !csrfOrigins(selection))
      throw new Error("Enter at most 16 distinct HTTPS origins with no paths.");
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 || current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("CSRF origin policy revision cannot be updated.");
    const update = await api.post<unknown>("management_settings/update", {
      expected_revision: current.revision, csrf_allowed_origins: selection,
    });
    if (update && typeof update === "object" && (update as Record<string, unknown>).code === "revision_conflict")
      throw new Error("Management policy changed in another editor. Refresh and try again.");
    const confirmed = parseCsrfPolicyReply(update);
    if (confirmed.revision !== current.revision + 1 || !sameCsrfOrigins(confirmed.values.csrf_allowed_origins, selection))
      throw new Error("CSRF origin policy update could not be confirmed. Refresh and check the current value.");
    const saved = parseCsrfPolicyReply(await api.post<unknown>("management_settings/get"));
    if (saved.revision !== confirmed.revision || !sameCsrfOrigins(saved.values.csrf_allowed_origins, selection))
      throw new Error("CSRF origin policy could not be confirmed after saving. Refresh and check the current value.");
    return saved;
  }

  export function renderCsrfPolicy(context: PageContext): void {
    const area = el("div");
    let generation = 0;
    context.content.append(area);
    async function load(): Promise<void> {
      const request = ++generation;
      area.replaceChildren(el("p", "muted", "Loading CSRF origin policy…"));
      try {
        const current = parseCsrfPolicyReply(await context.api.post<unknown>("management_settings/get"));
        if (!context.content.isConnected || request !== generation) return;
        const mode = el("select");
        mode.name = "csrf_allowed_origins_mode";
        for (const [value, label] of [["default", "Default (no exceptions)"], ["custom", "Explicit origin list"]]) {
          const option = el("option", "", label);
          option.value = value ?? "";
          mode.append(option);
        }
        mode.value = current.values.csrf_allowed_origins === null ? "default" : "custom";
        const origins = el("textarea");
        origins.name = "csrf_allowed_origins";
        origins.rows = 4;
        origins.value = current.csrf_allowed_origins.join("\n");
        const form = el("form", "settings-form");
        const status = el("div");
        const save = el("button", "button primary", "Save CSRF origins");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        mode.disabled = save.disabled;
        const updateAvailability = (): void => {
          origins.disabled = save.disabled || mode.value !== "custom";
        };
        updateAvailability();
        mode.addEventListener("change", updateAvailability);
        form.append(field("CSRF origin exceptions", mode,
          "Listed origins may use signed-in cookies without the usual CSRF header. Add only trusted HTTPS origins. Sign-in and the host's separate browser access policy still apply."),
          field("Exact HTTPS origins, one per line", origins,
            "Include a port when it appears in the browser origin. Paths and wildcards are rejected. An empty explicit list disables all exceptions."), save, status);
        form.addEventListener("submit", event => {
          event.preventDefault();
          if (save.disabled || !context.api.allows("configuration.write")) return;
          ++generation;
          mode.disabled = true;
          origins.disabled = true;
          void runAction(save, status, async () => {
            const selection = mode.value === "default" ? null : mode.value === "custom" ?
              origins.value.split(/\r?\n/).map(value => value.trim()).filter(Boolean) : undefined;
            try {
              if (selection === undefined) throw new Error("Choose a valid CSRF origin policy.");
              await saveCsrfPolicy(context.api, current, selection);
              context.notify("CSRF origin policy saved.");
              await load();
            } catch (error) {
              await load();
              if (context.content.isConnected) area.append(errorPanel(error));
              throw error;
            }
          }).finally(() => {
            mode.disabled = !context.api.allows("configuration.write");
            updateAvailability();
          });
        });
        area.replaceChildren(panel("CSRF origin exceptions", button("Refresh CSRF origins", load), form));
      } catch (error) {
        if (context.content.isConnected && request === generation) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
}
