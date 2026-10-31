namespace Amaterasu {
  interface ManagedApiToken {
    token_id: string;
    scopes: string[];
    issued_at: number;
    expires_at: number;
  }

  function tokenRecord(value: unknown): ManagedApiToken {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The host returned an invalid token list.");
    const record = value as Record<string, unknown>;
    if (typeof record.token_id !== "string" || !/^[a-f0-9]{64}$/.test(record.token_id) ||
        !Array.isArray(record.scopes) || record.scopes.length === 0 || record.scopes.length > 64 ||
        !record.scopes.every((scope) => typeof scope === "string" && /^[a-zA-Z0-9._:/-]{1,64}$/.test(scope)) ||
        !Number.isSafeInteger(record.issued_at) || !Number.isSafeInteger(record.expires_at) ||
        (record.issued_at as number) < 0 || (record.expires_at as number) <= (record.issued_at as number))
      throw new Error("The host returned an invalid token list.");
    return { token_id: record.token_id, scopes: record.scopes as string[],
      issued_at: record.issued_at as number, expires_at: record.expires_at as number };
  }

  export function renderTokens(context: PageContext): void {
    const listArea = el("div");
    const secretArea = el("div");
    let secretNode: HTMLTextAreaElement | null = null;
    const createStatus = el("div");
    const scopeSearch = input("token_scope_search", "", "search");
    scopeSearch.placeholder = "Filter permissions";
    const tokenSearch = input("token_search", "", "search");
    tokenSearch.placeholder = "Search permissions or token ID";
    const scopeList = el("div", "token-scope-list");
    scopeList.setAttribute("role", "group");
    scopeList.setAttribute("aria-label", "Available token permissions");
    const scopeSummary = el("p", "muted token-scope-summary");
    scopeSummary.setAttribute("role", "status");
    const allowed = Array.from(new Set(context.api.scopes ?? []))
      .filter((scope) => scope !== "tokens.manage" && /^[a-zA-Z0-9._:/-]{1,64}$/.test(scope)).sort();
    const checks = allowed.map((scope) => {
      const checkbox = input(`token_scope_${scope}`, "", "checkbox");
      const label = el("label", "token-scope", checkbox, el("span", "", literalText(scope)));
      scopeList.append(label);
      return { scope, checkbox, label };
    });
    function updateScopes(): void {
      const query = scopeSearch.value.trim().toLocaleLowerCase();
      let visible = 0;
      for (const entry of checks) {
        entry.label.hidden = !entry.scope.toLocaleLowerCase().includes(query);
        if (!entry.label.hidden) visible++;
      }
      const selected = checks.filter((entry) => entry.checkbox.checked).length;
      scopeSummary.textContent = `${selected} selected · ${visible} of ${checks.length} permissions${visible > 5 ? " · Scroll to see more" : ""}`;
    }
    scopeSearch.addEventListener("input", updateScopes);
    for (const entry of checks) entry.checkbox.addEventListener("change", updateScopes);
    updateScopes();
    const expiry = el("select");
    expiry.name = "token_lifetime";
    for (const [seconds, label] of [["3600", "1 hour"], ["86400", "1 day"], ["604800", "7 days"], ["2592000", "30 days"]]) {
      const option = el("option", "", label); option.value = seconds ?? ""; expiry.append(option);
    }
    expiry.value = "86400";
    const create = el("button", "button primary", "Create token");
    create.type = "submit";
    const form = el("form", "settings-form", field("Filter permissions", scopeSearch), scopeSummary, scopeList,
      field("Expires after", expiry), create, createStatus);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (secretNode) {
        createStatus.replaceChildren(errorPanel(new Error("Copy or dismiss the current token before creating another.")));
        return;
      }
      const scopes = checks.filter((entry) => entry.checkbox.checked).map((entry) => entry.scope);
      if (!scopes.length) {
        createStatus.replaceChildren(errorPanel(new Error("Select at least one permission.")));
        return;
      }
      void runAction(create, createStatus, async () => {
        // A lost response may have committed. Never replay an uncertain issuance.
        const reply = await context.api.post<unknown>("tokens/issue", { scopes, ttl_seconds: Number(expiry.value) });
        if (!reply || typeof reply !== "object" || Array.isArray(reply)) throw new Error("Token creation was not confirmed. Refresh the list before trying again.");
        const result = reply as Record<string, unknown>;
        if (result.ok !== true || typeof result.bearer !== "string" || !/^ama_[a-f0-9]{64}$/.test(result.bearer) ||
            typeof result.token_id !== "string" || !/^[a-f0-9]{64}$/.test(result.token_id) ||
            !Array.isArray(result.scopes) || result.scopes.length !== scopes.length ||
            new Set(result.scopes).size !== scopes.length || !result.scopes.every((scope) => typeof scope === "string" && scopes.includes(scope)) ||
            !Number.isSafeInteger(result.expires_at) || (result.expires_at as number) <= 0)
          throw new Error("Token creation was not confirmed. Refresh the list before trying again.");
        if (!context.content.isConnected) return;
        const secret = el("textarea", "token-secret");
        secret.readOnly = true;
        secret.spellcheck = false;
        secret.value = result.bearer;
        secretNode = secret;
        secret.setAttribute("aria-label", "New API token");
        const copy = button("Copy token", () => void runAction(copy, createStatus, async () => {
          if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable in this browser.");
          await navigator.clipboard.writeText(secret.value);
          context.notify("Token copied.");
        }));
        const dismiss = button("Done", () => { secret.value = ""; secretNode = null; secretArea.replaceChildren(); });
        secretArea.replaceChildren(panel("Copy your new token", el("p", "notice", "This secret is shown only once. Store it somewhere safe before leaving this page."),
          secret, el("div", "actions", copy, dismiss)));
        for (const entry of checks) entry.checkbox.checked = false;
        updateScopes();
        await load();
      });
    });
    context.content.append(sectionHeading("API tokens", "Create scoped credentials for tools that access this host."), secretArea,
      panel("Create token", el("p", "muted", "Choose only the permissions this tool needs. Tokens expire automatically."), form),
      panel("Issued tokens", el("p", "muted", "Only tokens created through this management interface are shown. Other native callers manage their own tokens."),
        el("div", "diagnostics-toolbar", field("Search issued tokens", tokenSearch), button("Refresh tokens", load)), listArea));
    context.registerDraftGuard?.(
      () => secretNode !== null || checks.some((entry) => entry.checkbox.checked),
      () => { if (secretNode) secretNode.value = ""; secretNode = null; },
    );
    let generation = 0;
    let records: ManagedApiToken[] = [];
    function renderList(): void {
      const query = tokenSearch.value.trim().toLocaleLowerCase();
      const visible = records.filter((token) => token.token_id.includes(query) || token.scopes.some((scope) => scope.toLocaleLowerCase().includes(query)));
      if (!visible.length) {
        listArea.replaceChildren(empty(records.length ? "No matching tokens" : "No issued tokens", records.length ? "Try another search." : "Create a token when a tool needs access."));
        return;
      }
      const rows = visible.map((token) => {
        const state = el("div");
        const remove = button("Revoke", () => {
          if (!window.confirm(`Revoke token ${token.token_id.slice(0, 8)}…? Tools using it will lose access.`)) return;
          void runAction(remove, state, async () => {
            await context.api.post("tokens/revoke", { token_id: token.token_id });
            context.notify("Token revoked.");
            await load();
          });
        }, "quiet danger-text");
        return el("section", "token-card", el("div", "section-meta",
          el("strong", "", literalText(`${token.token_id.slice(0, 8)}…${token.token_id.slice(-4)}`)), remove),
          el("p", "muted", literalText(`Created ${new Date(token.issued_at * 1000).toLocaleString()} · Expires ${new Date(token.expires_at * 1000).toLocaleString()}`)),
          el("div", "token-badges", ...token.scopes.map((scope) => badge(literalText(scope)))), state);
      });
      listArea.replaceChildren(...rows);
    }
    tokenSearch.addEventListener("input", renderList);
    async function load(): Promise<void> {
      const current = ++generation;
      try {
        const reply = await context.api.post<{ tokens: unknown }>("tokens/list");
        if (!context.content.isConnected || current !== generation) return;
        if (!Array.isArray(reply.tokens) || reply.tokens.length > 32) throw new Error("The host returned an invalid token list.");
        records = reply.tokens.map(tokenRecord).sort((a, b) => b.issued_at - a.issued_at);
        renderList();
      } catch (error) {
        if (context.content.isConnected && current === generation) listArea.replaceChildren(errorPanel(error, load));
      }
    }
    if (!allowed.length) scopeList.append(el("p", "muted", "No permissions are available for this session."));
    listArea.replaceChildren(el("p", "muted", "Loading issued tokens…"));
    void load();
  }
}
