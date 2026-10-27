namespace Amaterasu {
  type AccountRole = "administrator" | "operator" | "viewer";
  interface AccountRecord { username: string; role: AccountRole; enabled: boolean }
  function roleSelector(): HTMLSelectElement {
    const select = el("select");
    for (const role of ["administrator", "operator", "viewer"]) { const option = el("option", "", role); option.value = role; select.append(option); }
    return select;
  }
  export function renderAccounts(context: PageContext): void {
    const area = el("div");
    const current = input("current_password", "", "password"); current.autocomplete = "current-password"; current.required = true;
    const next = input("new_password", "", "password"); next.autocomplete = "new-password"; next.required = true;
    const passwordStatus = el("div"); const change = el("button", "button primary", "Change password"); change.type = "submit";
    const passwordForm = el("form", "settings-form", field("Current password", current), field("New password", next), change, passwordStatus);
    passwordForm.addEventListener("submit", (event) => { event.preventDefault(); if (!passwordForm.reportValidity()) return;
      void runAction(change, passwordStatus, async () => { await context.api.post("auth/change_password", { current_password: current.value, new_password: next.value }); current.value = ""; next.value = ""; context.notify("Password changed."); }); });
    context.content.append(el("h1", "", "Accounts"));
    if (context.api.allows("account.self")) context.content.append(panel("Your password", passwordForm));
    context.content.append(area);
    async function load(): Promise<void> {
      area.replaceChildren(el("p", "muted", "Loading accounts…"));
      try {
        const reply = await context.api.post<{ accounts: AccountRecord[] }>("accounts/list");
        if (!context.content.isConnected) return;
        if (!Array.isArray(reply.accounts)) throw new Error("Invalid account list.");
        const list = el("div");
        for (const account of reply.accounts) {
          if (typeof account.username !== "string" || !["administrator", "operator", "viewer"].includes(account.role) || typeof account.enabled !== "boolean") throw new Error("Invalid account list.");
          const role = roleSelector(); role.value = account.role;
          const enabled = input("enabled", "", "checkbox"); enabled.checked = account.enabled;
          const status = el("div");
          const save = button("Save", () => void runAction(save, status, async () => { await context.api.post("accounts/update", { username: account.username, role: role.value, enabled: enabled.checked }); context.notify("Account updated."); await load(); }));
          const reset = input("password", "", "password"); reset.autocomplete = "new-password";
          const setPassword = button("Set password", () => void runAction(setPassword, status, async () => { if (!reset.value) throw new Error("Enter a new password."); await context.api.post("accounts/set_password", { username: account.username, password: reset.value }); reset.value = ""; context.notify("Password updated."); }));
          const remove = button("Delete account", () => { if (!window.confirm(`Delete account ${account.username}?`)) return; void runAction(remove, status, async () => { await context.api.post("accounts/delete", { username: account.username }); await load(); }); });
          list.append(panel(account.username, field("Role", role), field("Enabled", enabled), save, field("New password", reset), setPassword, remove, status));
        }
        const username = input("username"); username.required = true; username.autocomplete = "off";
        const password = input("password", "", "password"); password.required = true; password.autocomplete = "new-password";
        const role = roleSelector(); role.value = "viewer";
        const status = el("div"); const create = el("button", "button primary", "Create account"); create.type = "submit";
        const form = el("form", "settings-form", field("Username", username), field("Password", password), field("Role", role), create, status);
        form.addEventListener("submit", (event) => { event.preventDefault(); if (!form.reportValidity()) return; void runAction(create, status, async () => { await context.api.post("accounts/create", { username: username.value.trim(), password: password.value, role: role.value }); password.value = ""; context.notify("Account created."); await load(); }); });
        area.replaceChildren(panel("Manage accounts", el("p", "muted", "Accounts share this host's resources. Permissions are enforced by the host."), list, form));
      } catch (error) { if (context.content.isConnected) { if (error instanceof ApiError && error.status === 403) area.replaceChildren(); else area.replaceChildren(errorPanel(error, load)); } }
    }
    if (context.api.allows("configuration.write")) void load();
  }
}
