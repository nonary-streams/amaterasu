namespace Amaterasu {
  function managementPolicyTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  export type ManagementPeerTier = "pc" | "lan" | "wan";
  export interface ManagementPolicyReply {
    ok: true;
    revision: number;
    values: { origin_web_ui_allowed: ManagementPeerTier | null };
    origin_web_ui_allowed: ManagementPeerTier;
  }

  function managementTier(value: unknown): value is ManagementPeerTier {
    return value === "pc" || value === "lan" || value === "wan";
  }

  export function parseManagementPolicyReply(value: unknown): ManagementPolicyReply {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Management access policy reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.revision !== "number" ||
        !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
        reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("Management access policy reply is invalid.");
    const values = reply.values as Record<string, unknown>;
    const selection = values.origin_web_ui_allowed;
    if ((selection !== null && !managementTier(selection)) ||
        !managementTier(reply.origin_web_ui_allowed) ||
        reply.origin_web_ui_allowed !== (selection ?? "lan"))
      throw new Error("Management access policy was not confirmed by the host.");
    return { ok: true, revision: reply.revision,
      values: { origin_web_ui_allowed: selection },
      origin_web_ui_allowed: reply.origin_web_ui_allowed };
  }

  export async function saveManagementPolicy(
    api: Pick<Api, "post">,
    current: ManagementPolicyReply,
    selection: ManagementPeerTier | null,
  ): Promise<ManagementPolicyReply> {
    if (selection !== null && !managementTier(selection))
      throw new Error("Choose a valid management access policy.");
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
        current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("Management access policy revision cannot be updated.");
    const update = await api.post<unknown>("management_settings/update", {
      expected_revision: current.revision, origin_web_ui_allowed: selection,
    });
    if (update && typeof update === "object" &&
        (update as Record<string, unknown>).code === "revision_conflict")
      throw new Error("Management access policy changed in another editor. Refresh and try again.");
    const confirmed = parseManagementPolicyReply(update);
    if (confirmed.revision !== current.revision + 1 ||
        confirmed.values.origin_web_ui_allowed !== selection)
      throw new Error("Management access policy update could not be confirmed. Refresh and check the current value.");
    const saved = parseManagementPolicyReply(await api.post<unknown>("management_settings/get"));
    if (saved.revision !== confirmed.revision || saved.values.origin_web_ui_allowed !== selection)
      throw new Error("Management access policy could not be confirmed after saving. Refresh and check the current value.");
    return saved;
  }

  export function renderManagementPolicy(context: PageContext): void {
    const area = el("div");
    let generation = 0;
    context.content.append(area);
    async function load(): Promise<void> {
      const request = ++generation;
      area.replaceChildren(el("p", "muted", "Loading management access policy…"));
      try {
        const current = parseManagementPolicyReply(await context.api.post<unknown>("management_settings/get"));
        if (!context.content.isConnected || request !== generation) return;
        const selection = el("select");
        selection.name = "origin_web_ui_allowed";
        for (const [value, label] of [
          ["", "Default (this PC and LAN)"],
          ["pc", "This PC"],
          ["lan", "This PC and LAN"],
          ["wan", "All networks"],
        ]) {
          const option = el("option", "", label);
          option.value = value ?? "";
          selection.append(option);
        }
        selection.value = current.values.origin_web_ui_allowed ?? "";
        const form = el("form", "settings-form");
        const status = el("div");
        const save = el("button", "button primary", "Save management access");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        selection.disabled = save.disabled;
        form.append(field("Allow management access from", selection,
          "Remote access uses the host's configured HTTPS listener. Administrator sign-in is still required. Choosing this PC can disconnect a remote browser."), save, status);
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          if (save.disabled || !context.api.allows("configuration.write")) return;
          ++generation;
          selection.disabled = true;
          void runAction(save, status, async () => {
            const value = selection.value;
            if (value !== "" && !managementTier(value)) throw new Error("Choose a valid management access policy.");
            try {
              await saveManagementPolicy(context.api, current, value === "" ? null : value);
              context.notify(managementPolicyTextMessage("Management access policy saved."));
              await load();
            } catch (error) {
              await load();
              if (context.content.isConnected) area.append(errorPanel(error));
              throw error;
            }
          }).finally(() => { selection.disabled = !context.api.allows("configuration.write"); });
        });
        area.replaceChildren(panel("Management access", button("Refresh policy", load), form));
      } catch (error) {
        if (context.content.isConnected && request === generation)
          area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
}
