namespace Amaterasu {
  interface BrowserSessionView {
    id: string;
    subject: string;
    expiresAt: number;
    refreshExpiresAt: number;
    remembered: boolean;
    current: boolean;
  }

  function parseBrowserSessions(value: unknown): BrowserSessionView[] {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The host returned an invalid session list.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Array.isArray(reply.sessions) || reply.sessions.length > 16)
      throw new Error("The host returned an invalid session list.");
    const ids = new Set<string>();
    const sessions = reply.sessions.map((value: unknown): BrowserSessionView => {
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The host returned an invalid session list.");
      const session = value as Record<string, unknown>;
      if (typeof session.session_id !== "string" || !/^[a-f0-9]{64}$/.test(session.session_id) || ids.has(session.session_id) ||
          typeof session.subject !== "string" || session.subject.length < 1 || session.subject.length > 64 ||
          !/^[a-zA-Z0-9._:/-]+$/.test(session.subject) || typeof session.remember_me !== "boolean" || typeof session.current !== "boolean" ||
          !Number.isSafeInteger(session.expires_at) || !Number.isSafeInteger(session.refresh_expires_at) ||
          (session.expires_at as number) < 0 || (session.refresh_expires_at as number) < (session.expires_at as number) ||
          (session.refresh_expires_at as number) > 8_640_000_000)
        throw new Error("The host returned an invalid session list.");
      ids.add(session.session_id);
      return { id: session.session_id, subject: session.subject, expiresAt: session.expires_at as number,
        refreshExpiresAt: session.refresh_expires_at as number, remembered: session.remember_me, current: session.current };
    });
    if (sessions.filter((session) => session.current).length !== 1) throw new Error("The current session could not be identified.");
    return sessions.sort((left, right) => Number(right.current) - Number(left.current) || right.refreshExpiresAt - left.refreshExpiresAt);
  }

  export function renderMaintenance(context: PageContext): void {
    const sessionsArea = el("div");
    const actionStatus = el("div");
    let generation = 0;
    context.content.append(
      sectionHeading("Maintenance", "Review signed-in sessions and manage this host's credentials and software.", button("Refresh sessions", () => { void load(); })),
      panel("Signed-in sessions", el("p", "muted", "Revoke another session to remove its access. Sessions may include other management clients; the host does not record device names or last activity."), actionStatus, sessionsArea),
      panel("Credentials", el("p", "muted", "Manage accounts, passwords, and API tokens for this host."),
        el("div", "actions", button("Manage accounts", () => context.navigate("accounts")),
          ...(context.api.allows("tokens.manage") ? [button("Manage API tokens", () => context.navigate("tokens"))] : []))),
      panel("Plugins", el("p", "muted", "Review the running package and this host's repositories."),
        context.api.allows("configuration.read") ? button("View plugins", () => context.navigate("plugins")) : el("p", "muted", "Package details require configuration.read.")),
    );

    function render(sessions: BrowserSessionView[]): void {
      const cards = sessions.map((session) => {
        const title = el("div", "section-meta", el("strong", "", literalText(session.subject)),
          session.current ? badge("This session", "good") : session.remembered ? badge("Remembered") : badge("Session only"));
        const details = el("p", "muted", literalText(`Access expires ${new Date(session.expiresAt * 1000).toLocaleString()} · Refresh expires ${new Date(session.refreshExpiresAt * 1000).toLocaleString()}`));
        if (session.current) return el("section", "browser-session-card", title, details);
        const revoke = button("Revoke session", () => {
          if (!window.confirm(`Revoke this session for ${session.subject}? It will lose access on its next request.`)) return;
          void runAction(revoke, actionStatus, async () => {
            let acknowledged: boolean;
            try {
              const reply = await context.api.post<{ revoked: unknown }>("account-sessions/revoke", { session_id: session.id });
              if (typeof reply.revoked !== "boolean") throw new Error("The host did not confirm the session action.");
              acknowledged = reply.revoked;
            } catch (error) {
              const latest = await load();
              if (latest && !latest.some((candidate) => candidate.id === session.id)) {
                actionStatus.replaceChildren(el("p", "notice", "This session is no longer listed. The revoke response was interrupted, so its cause could not be confirmed."));
                return;
              }
              throw error;
            }
            const latest = await load();
            if (!latest || latest.some((candidate) => candidate.id === session.id))
              throw new Error("The session's removal could not be confirmed. Refresh before retrying.");
            context.notify(acknowledged ? "Session revoked." : "That session had already ended.");
          });
        }, "quiet danger-text");
        revoke.setAttribute("aria-label", `Revoke management session for ${session.subject}, refresh expires ${new Date(session.refreshExpiresAt * 1000).toLocaleString()}`);
        return el("section", "browser-session-card", title, details, revoke);
      });
      sessionsArea.replaceChildren(...cards);
    }
    async function load(): Promise<BrowserSessionView[] | null> {
      const request = ++generation;
      try {
        const sessions = parseBrowserSessions(await context.api.post<unknown>("account-sessions/list"));
        if (!context.content.isConnected || request !== generation) return null;
        render(sessions);
        return sessions;
      } catch (error) {
        if (context.content.isConnected && request === generation) sessionsArea.replaceChildren(errorPanel(error, () => { void load(); }));
        return null;
      }
    }
    sessionsArea.replaceChildren(el("p", "muted", "Loading signed-in sessions…"));
    void load();
  }
}
