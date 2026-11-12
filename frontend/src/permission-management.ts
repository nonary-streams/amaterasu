namespace Amaterasu {
  interface ClientPermissions { list_apps: boolean; view_streams: boolean; launch_apps: boolean; terminate_streams: boolean }
  interface PermissionClient { certificate_id: string; name: string; permissions: ClientPermissions }
  interface PermissionState { revision: number; permission_mode_enabled: boolean; clients: PermissionClient[] }
  const permissionKeys: (keyof ClientPermissions)[] = ["list_apps", "view_streams", "launch_apps", "terminate_streams"];

  export function parseClientPermissionState(value: unknown): PermissionState {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid client permissions response.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        typeof reply.permission_mode_enabled !== "boolean" || !Array.isArray(reply.clients))
      throw new Error("Client permissions are unavailable on this host.");
    const seen = new Set<string>();
    const clients = reply.clients.map((item: unknown): PermissionClient => {
      if (!item || typeof item !== "object" || Array.isArray(item)) throw new Error("Invalid client permissions response.");
      const client = item as Record<string, unknown>;
      if (typeof client.certificate_id !== "string" || !/^[0-9a-fA-F]{64}$/.test(client.certificate_id) ||
          seen.has(client.certificate_id) || typeof client.name !== "string" || !client.permissions ||
          typeof client.permissions !== "object" || Array.isArray(client.permissions))
        throw new Error("Invalid client permissions response.");
      const permissions = client.permissions as Record<string, unknown>;
      if (Object.keys(permissions).length !== permissionKeys.length || permissionKeys.some((key) => typeof permissions[key] !== "boolean"))
        throw new Error("This host reports permissions that this editor cannot safely preserve.");
      seen.add(client.certificate_id);
      return { certificate_id: client.certificate_id, name: client.name, permissions: permissions as unknown as ClientPermissions };
    });
    return { revision: reply.revision as number, permission_mode_enabled: reply.permission_mode_enabled, clients };
  }

  export function renderClientPermissions(context: PageContext, area: HTMLElement, onSaveBusy: (busy: boolean) => void = () => {}): void {
    if (!context.api.allows("pairing.admin")) return;
    let generation = 0;
    async function load(): Promise<void> {
      const request = ++generation;
      area.replaceChildren(el("p", "muted", "Loading client permissions…"));
      try {
        const state = parseClientPermissionState(await context.api.post<unknown>("pairing/list_clients"));
        if (!context.content.isConnected || request !== generation) return;
        const body = el("div"), status = el("div");
        const baseline = new Map<HTMLInputElement, boolean>();
        const clientCards: { card: HTMLElement; searchText: string }[] = [];
        const mode = input("permission_mode_enabled", "", "checkbox");
        mode.checked = state.permission_mode_enabled;
        baseline.set(mode, mode.checked);
        const saveMode = button("Save permission mode", () => void save("pairing/set_permission_mode", { enabled: mode.checked },
          (saved) => saved.permission_mode_enabled === mode.checked, [mode]));
        body.append(field("Enforce client permissions", mode,
          "When enabled, listing and launching applications are controlled by these grants. Disabled by default."), saveMode);
        if (state.permission_mode_enabled) {
          body.append(el("p", "muted", "Launch and the stored view grant also allow listing. The view streams permission currently grants listing, not separate stream viewing. Termination is not enforced yet; its stored grant is preserved."));
          const search = input("client_permission_search", "", "search");
          search.setAttribute("data-draft-ignore", "");
          const resultCount = el("p", "muted");
          resultCount.setAttribute("role", "status");
          resultCount.setAttribute("aria-live", "polite");
          resultCount.setAttribute("aria-atomic", "true");
          const noMatches = el("p", "muted", "No paired clients match this search.");
          noMatches.hidden = true;
          const updateClientFilter = (): void => {
            const query = search.value.trim().toLocaleLowerCase();
            let visible = 0;
            for (const item of clientCards) {
              item.card.hidden = !!query && !item.searchText.includes(query);
              if (!item.card.hidden) visible += 1;
            }
            resultCount.textContent = `Showing ${visible} of ${clientCards.length} paired clients`;
            noMatches.hidden = visible !== 0;
          };
          search.addEventListener("input", updateClientFilter);
          body.append(field("Search paired clients by name or certificate fingerprint", search), resultCount, noMatches);
          for (const client of state.clients) {
            const checks = {} as Record<"list_apps" | "view_streams" | "launch_apps", HTMLInputElement>;
            const rows = el("div", "form-grid");
            for (const [key, label] of [["list_apps", "List applications"], ["view_streams", "Additional listing grant (view streams permission)"], ["launch_apps", "Launch applications"]] as const) {
              const check = input(`${key}_${client.certificate_id}`, "", "checkbox");
              check.checked = client.permissions[key];
              checks[key] = check;
              baseline.set(check, check.checked);
              rows.append(field(label, check));
            }
            const saveClient = button("Save permissions", () => {
              const permissions: ClientPermissions = { ...client.permissions, list_apps: checks.list_apps.checked, view_streams: checks.view_streams.checked, launch_apps: checks.launch_apps.checked };
              void save("pairing/update_client_permissions", { certificate_id: client.certificate_id, permissions }, (saved) => {
                const updated = saved.clients.find((item) => item.certificate_id === client.certificate_id);
                return !!updated && permissionKeys.every((key) => updated.permissions[key] === permissions[key]);
              }, Object.values(checks));
            });
            const name = typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(client.name) : client.name;
            const effective = el("p", "muted");
            const updateEffective = () => { effective.textContent = checks.list_apps.checked || checks.view_streams.checked || checks.launch_apps.checked
              ? "Effective listing: allowed." : "Effective listing: denied."; };
            for (const check of Object.values(checks)) check.addEventListener("change", updateEffective);
            updateEffective();
            const card = el("section", "account-card", el("h3", "", name), effective, rows, saveClient);
            clientCards.push({ card, searchText: `${client.name} ${client.certificate_id}`.toLocaleLowerCase() });
            body.append(card);
          }
          updateClientFilter();
          if (!state.clients.length) body.append(el("p", "muted", "No paired clients."));
        }
        const refresh = button("Refresh permissions", () => {
          if (window.confirm("Reload client permissions and discard unsaved changes?")) void load();
        });
        area.replaceChildren(panel("Client permissions", refresh, body, status));
        async function save(path: string, payload: Record<string, unknown>, matches: (saved: PermissionState) => boolean, submitted: HTMLInputElement[]): Promise<void> {
          const otherDrafts = Array.from(baseline).some(([control, value]) => !submitted.includes(control) && control.checked !== value);
          if (otherDrafts && !window.confirm("Saving will reload and discard other unsaved permission changes. Continue?")) return;
          for (const control of Array.from(body.querySelectorAll<HTMLInputElement | HTMLButtonElement>("input, button"))) control.disabled = true;
          refresh.disabled = true;
          onSaveBusy(true);
          status.replaceChildren(el("p", "muted", "Saving client permissions…"));
          try {
            await context.api.post(path, { ...payload, expected_revision: state.revision });
            const saved = parseClientPermissionState(await context.api.post<unknown>("pairing/list_clients"));
            if (saved.revision !== state.revision + 1 || !matches(saved)) throw new Error("The saved permissions could not be confirmed.");
            context.notify("Client permissions saved.");
            await load();
          } catch (error) {
            if (!context.content.isConnected) return;
            status.replaceChildren(errorPanel(error), el("p", "muted", "Permissions may have changed. Refresh to review the latest state before trying again."));
            refresh.disabled = false;
          } finally {
            onSaveBusy(false);
          }
        }
      } catch (error) {
        if (context.content.isConnected && request === generation) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
}
