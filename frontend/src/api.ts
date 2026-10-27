namespace Amaterasu {
  type SessionReply = LoginReply & {
    refresh_token: string;
    refresh_expires_at: number;
    remember_me: boolean;
  };

  type ValidatedSession = {
    bearer: string;
    refresh_token: string;
    subject: string;
    role: AccountRole | null;
    scopes: string[];
    expires_at: number;
    refresh_expires_at: number;
    remember_me: boolean;
  };

  type StoredSession = ValidatedSession & {
    version: 1;
    base: string;
    session_id: string;
    refreshing: boolean;
  };

  type AccountRole = "administrator" | "operator" | "viewer";

  type BrowserSession = {
    ok: true;
    subject: string;
    role?: AccountRole;
    scopes: string[];
    expires_at: number;
  };

  const SESSION_STORAGE_VERSION = 1;

  export class ApiError extends Error {
    constructor(
      message: string,
      public readonly code: string,
      public readonly status: number,
    ) {
      super(message);
    }
  }

  export class Api {
    private bearer = "";
    private refreshToken = "";
    private accessExpiresAt = 0;
    private authGeneration = 0;
    private persistenceGeneration = 0;
    private tokenGeneration = 0;
    private refreshInFlight: Promise<void> | null = null;
    private logoutInFlight: Promise<void> | null = null;
    private requests = new Set<AbortController>();
    private refreshRequests = new Set<AbortController>();
    private readonly base: string;
    private readonly storageKey: string;
    private readonly refreshLockName: string;
    private readonly cookieLogoutKey: string;
    private readonly cookiePendingLogoutKey: string;
    readonly usesSecureCookies: boolean;
    private cookieSession = false;
    private cookieRefreshInFlight: Promise<void> | null = null;
    private remembered = false;
    private sessionId: string | null = null;
    private loggingOut = false;
    onAuthenticationLost: (() => void) | null = null;
    subject = "";
    role: AccountRole | "token" | null = null;
    scopes: string[] | null = null;

    constructor(base: string) {
      const url = new URL(base || location.origin, location.origin);
      if (
        !["http:", "https:"].includes(url.protocol) ||
        url.username ||
        url.password ||
        url.search ||
        url.hash
      )
        throw new Error("The API address must be an HTTP(S) origin.");
      this.base = url.origin;
      this.usesSecureCookies =
        url.protocol === "https:" && this.base === location.origin;
      this.storageKey = `amaterasu.session.${encodeURIComponent(this.base)}`;
      this.refreshLockName = `amaterasu.session.refresh:${this.base}`;
      this.cookieLogoutKey = `amaterasu.session.cookie-logout.${encodeURIComponent(this.base)}`;
      this.cookiePendingLogoutKey = `amaterasu.session.cookie-pending-logout.${encodeURIComponent(this.base)}`;
      if (
        typeof window !== "undefined" &&
        typeof window.addEventListener === "function"
      ) {
        window.addEventListener("storage", (event) =>
          this.handleStorageChange(event as StorageEvent),
        );
      }
    }

    async login(
      username: string,
      password: string,
      rememberMe = false,
    ): Promise<void> {
      if (this.loggingOut) {
        throw new ApiError(
          "Sign out is already in progress.",
          "unauthorized",
          401,
        );
      }
      if (this.usesSecureCookies) {
        await this.loginWithCookies(username, password, rememberMe);
        return;
      }
      if (rememberMe && !this.webLocks())
        throw this.rememberedRefreshLockError();
      const generation = this.authGeneration;
      const reply = await this.post<SessionReply>(
        "auth/login",
        { username, password, remember_me: rememberMe },
        false,
      );
      await this.installSession(reply, generation, true, rememberMe);
    }

    async restoreSession(): Promise<boolean> {
      if (this.loggingOut) return false;
      if (this.usesSecureCookies) return this.restoreCookieSession();
      this.resetMemory(true);
      if (!this.webLocks()) {
        // A remembered bearer cannot be used safely when this browser cannot
        // serialize refresh and sign-out with other tabs. Leave shared storage
        // untouched because another tab may be using or rotating its session.
        return false;
      }
      const generation = this.authGeneration;
      const persistenceGeneration = this.persistenceGeneration;
      const stored = await this.withSessionLock(() => {
        if (
          generation !== this.authGeneration ||
          persistenceGeneration !== this.persistenceGeneration
        )
          return null;
        const candidate = this.readStoredSession();
        if (
          candidate &&
          generation === this.authGeneration &&
          persistenceGeneration === this.persistenceGeneration
        )
          this.installStoredSession(candidate);
        return candidate;
      });
      if (!stored) return false;

      const token = this.refreshToken;
      if (stored.expires_at - Date.now() / 1000 <= 60)
        await this.refreshOrClear(generation, token, persistenceGeneration);
      return generation === this.authGeneration && this.bearer !== "";
    }

    suspend(): void {
      // Keep a pending marker if a remembered refresh was in flight. A reload
      // then clears it rather than replaying a refresh token that may be spent.
      this.resetMemory(false);
    }

    async useToken(token: string): Promise<void> {
      if (this.loggingOut) {
        throw new ApiError(
          "Sign out is already in progress.",
          "unauthorized",
          401,
        );
      }
      if (!/^ama_[a-f0-9]{64}$/.test(token))
        throw new Error(
          "Enter an Amaterasu token beginning with ama_ followed by 64 hexadecimal characters.",
        );
      await this.clear();
      this.bearer = token;
      this.subject = "API token";
      this.role = "token";
      this.scopes = null;
      this.tokenGeneration += 1;
    }

    async logout(): Promise<void> {
      if (this.logoutInFlight) return this.logoutInFlight;

      if (this.cookieSession) return this.logoutCookieSession();

      this.loggingOut = true;
      this.subject = "";
      this.role = null;
      this.scopes = null;
      this.abortRequests(true);

      const pendingLogout = this.finishLogout();
      this.logoutInFlight = pendingLogout;
      try {
        await pendingLogout;
      } finally {
        if (this.logoutInFlight === pendingLogout) this.logoutInFlight = null;
        this.loggingOut = false;
      }
    }

    private async finishLogout(): Promise<void> {
      const sessionId = this.remembered ? this.sessionId : null;
      const refreshTokens = new Set<string>();
      const originalRefreshToken = this.refreshToken;
      const originalBearerToken = originalRefreshToken ? "" : this.bearer;
      if (originalRefreshToken) refreshTokens.add(originalRefreshToken);

      // A rotating refresh can already have committed on the server. Let its
      // response settle so logout can revoke the replacement token too.
      const pendingRefresh = this.refreshInFlight;
      if (pendingRefresh) await pendingRefresh.catch(() => undefined);

      const sameSession =
        sessionId === null
          ? !this.remembered
          : this.remembered && this.sessionId === sessionId;
      if (sameSession && this.refreshToken)
        refreshTokens.add(this.refreshToken);
      const bearerToken =
        sameSession && !this.refreshToken ? this.bearer : originalBearerToken;

      this.resetMemory(true);

      const locks = this.webLocks();
      if (locks) {
        try {
          await locks.request(
            this.refreshLockName,
            { mode: "exclusive" },
            () => {
              // A refresh already holding the lock may have rotated. Revoke
              // the latest pair after it completes.
              const latest = this.readStoredSessionForCleanup(
                new Set<string>(),
                sessionId,
              );
              if (latest) {
                refreshTokens.add(latest.refresh_token);
                this.removeStoredSessionBestEffort(sessionId);
              }
            },
          );
        } catch {
          // Local sign-out is immediate even when lock cleanup cannot complete.
        }
      } else {
        this.removeStoredSessionBestEffort(sessionId);
      }

      for (const refreshToken of refreshTokens) {
        await this.revokeSession({ refresh_token: refreshToken });
      }
      if (bearerToken) await this.revokeSession({ bearer: bearerToken });
    }

    async clear(): Promise<void> {
      if (this.cookieSession) {
        await this.logoutCookieSession();
        return;
      }
      const sessionId = this.remembered ? this.sessionId : null;
      this.resetMemory(true);
      await this.removeStoredSessionForIdentity(sessionId);
    }

    allows(scope: string): boolean {
      if (this.scopes !== null && !this.scopes.includes(scope)) return false;
      if (this.role === "administrator" || this.role === "token") return true;
      // The server applies role restrictions in addition to scopes. Treat
      // missing role metadata as read-only until the session is refreshed.
      return ["account.self", "application.read", "catalog.read", "display.read",
        "lutris.read", "session.read", "steam.read"].includes(scope);
    }

    allowsLaunchAction(action: "start" | "stop"): boolean {
      const scope = action === "start" ? "application.launch" : "application.terminate";
      return (this.role === "administrator" || this.role === "operator" || this.role === "token") &&
        (this.scopes === null || this.scopes.includes(scope));
    }

    async post<T>(
      route: string,
      payload: Payload = {},
      protectedRoute = true,
    ): Promise<T> {
      if (this.loggingOut) {
        throw new ApiError(
          "Sign out is already in progress.",
          "unauthorized",
          401,
        );
      }
      if (protectedRoute && this.cookieSession)
        return this.postWithCookies<T>(route, payload);
      if (protectedRoute && !this.bearer)
        throw new ApiError("Sign in to continue.", "unauthorized", 401);

      if (
        protectedRoute &&
        this.refreshToken &&
        this.accessExpiresAt - Date.now() / 1000 <= 60
      ) {
        await this.refreshOrClear();
      }
      if (protectedRoute && !this.bearer)
        throw new ApiError("Sign in to continue.", "unauthorized", 401);

      const bearerUsed = this.bearer;
      const authGeneration = this.authGeneration;
      const tokenGeneration = this.tokenGeneration;
      try {
        const response = await this.send<T>(
          route,
          payload,
          protectedRoute,
          bearerUsed,
        );
        if (this.loggingOut) {
          throw new ApiError(
            "Sign out is already in progress.",
            "unauthorized",
            401,
          );
        }
        return response;
      } catch (error) {
        if (
          !protectedRoute ||
          !(error instanceof ApiError) ||
          error.code !== "unauthorized" ||
          this.loggingOut ||
          !this.refreshToken ||
          authGeneration !== this.authGeneration
        ) {
          throw error;
        }

        // A concurrent request or tab may already have rotated this bearer.
        // Reuse its pair instead of replaying a stale refresh token.
        if (tokenGeneration === this.tokenGeneration) await this.refreshOrClear();
        if (authGeneration !== this.authGeneration || !this.bearer) throw error;
        const response = await this.send<T>(route, payload, true, this.bearer);
        if (this.loggingOut) {
          throw new ApiError(
            "Sign out is already in progress.",
            "unauthorized",
            401,
          );
        }
        return response;
      }
    }

    private validateBrowserSession(reply: BrowserSession): BrowserSession {
      if (
        reply.ok !== true ||
        typeof reply.subject !== "string" ||
        !Array.isArray(reply.scopes) ||
        !reply.scopes.every((scope) => typeof scope === "string") ||
        !Number.isSafeInteger(reply.expires_at) ||
        reply.expires_at <= 0 ||
        Object.hasOwn(reply, "bearer") ||
        Object.hasOwn(reply, "refresh_token")
      )
        throw new ApiError(
          "The host returned an invalid authentication response.",
          "invalid_response",
          502,
        );
      return reply;
    }

    private applyBrowserSession(reply: BrowserSession): void {
      const session = this.validateBrowserSession(reply);
      this.cookieSession = true;
      this.accessExpiresAt = session.expires_at;
      this.subject = session.subject;
      this.role = session.role ?? null;
      this.scopes = [...session.scopes];
      this.tokenGeneration += 1;
    }

    private async loginWithCookies(
      username: string,
      password: string,
      rememberMe: boolean,
    ): Promise<void> {
      this.assertCookieStorageAvailable();
      if (!this.webLocks()) throw this.rememberedRefreshLockError();
      let generation = this.authGeneration;
      await this.withSessionLock(async () => {
        if (await this.finishPendingCookieLogout())
          throw new Error("The sign-in attempt was cancelled by a pending sign-out.");
        if (generation !== this.authGeneration)
          throw new Error("The sign-in attempt was cancelled.");
        // HttpOnly cookies cannot tell script whether an earlier session is
        // still present. Confirm its revocation before replacing the pair.
        await this.completeCookieLogout(this.markCookieLogoutPending());
        this.resetMemory(true);
        generation = this.authGeneration;
        const reply = await this.send<BrowserSession>(
          "auth/browser/login",
          { username, password, remember_me: rememberMe },
          false,
          "",
          true,
        );
        if (this.pendingCookieLogout()) {
          await this.finishPendingCookieLogout();
          throw new Error("The sign-in attempt was cancelled by a pending sign-out.");
        }
        if (generation !== this.authGeneration) {
          await this.completeCookieLogout(this.markCookieLogoutPending());
          throw new Error("The sign-in attempt was cancelled.");
        }
        try {
          this.applyBrowserSession(reply);
        } catch (error) {
          await this.completeCookieLogout(this.markCookieLogoutPending());
          throw error;
        }
        this.authGeneration += 1;
      });
    }

    private async restoreCookieSession(): Promise<boolean> {
      this.resetMemory(true);
      this.assertCookieStorageAvailable();
      if (!this.webLocks()) return false;
      const generation = this.authGeneration;
      return this.withSessionLock(async () => {
        if (await this.finishPendingCookieLogout()) return false;
        try {
          const reply = await this.readCookieSession();
          if (generation !== this.authGeneration || this.loggingOut)
            return false;
          this.applyBrowserSession(reply);
          return true;
        } catch (error) {
          if (error instanceof ApiError && error.status === 401)
            return false;
          throw error;
        }
      });
    }

    private async refreshCookieSession(): Promise<void> {
      if (this.cookieRefreshInFlight) return this.cookieRefreshInFlight;
      const generation = this.authGeneration;
      const pending = this.withSessionLock(async () => {
        if (generation !== this.authGeneration || this.loggingOut) return;
        const reply = await this.readCookieSession();
        if (generation === this.authGeneration && !this.loggingOut)
          this.applyBrowserSession(reply);
      });
      this.cookieRefreshInFlight = pending;
      try {
        await pending;
      } catch (error) {
        if (
          error instanceof ApiError &&
          (error.status === 401 || error.code === "logout_unconfirmed") &&
          generation === this.authGeneration && !this.loggingOut
        ) {
          this.resetMemory(true);
          this.onAuthenticationLost?.();
        }
        throw error;
      } finally {
        if (this.cookieRefreshInFlight === pending)
          this.cookieRefreshInFlight = null;
      }
    }

    private async postWithCookies<T>(route: string, payload: Payload): Promise<T> {
      if (this.pendingCookieLogout())
        throw new ApiError(
          "Sign-out is still pending. Retry when the host is reachable.",
          "logout_unconfirmed",
          503,
        );
      if (this.accessExpiresAt - Date.now() / 1000 <= 60)
        await this.refreshCookieSession();
      if (!this.cookieSession || this.loggingOut)
        throw new ApiError("Sign in to continue.", "unauthorized", 401);
      const generation = this.authGeneration;
      const tokenGeneration = this.tokenGeneration;
      try {
        const reply = await this.send<T>(route, payload, true, "", true);
        if (generation !== this.authGeneration || this.loggingOut)
          throw new ApiError("Sign in to continue.", "unauthorized", 401);
        return reply;
      } catch (error) {
        if (
          !(error instanceof ApiError) || error.status !== 401 ||
          generation !== this.authGeneration || this.loggingOut
        ) throw error;
        if (tokenGeneration === this.tokenGeneration)
          await this.refreshCookieSession();
        if (!this.cookieSession || generation !== this.authGeneration)
          throw error;
        try {
          const reply = await this.send<T>(route, payload, true, "", true);
          if (generation !== this.authGeneration || this.loggingOut)
            throw new ApiError("Sign in to continue.", "unauthorized", 401);
          return reply;
        } catch (retryError) {
          if (
            retryError instanceof ApiError &&
            retryError.status === 401 &&
            generation === this.authGeneration
          ) {
            this.resetMemory(true);
            this.onAuthenticationLost?.();
          }
          throw retryError;
        }
      }
    }

    private async requestCookieRefresh(): Promise<BrowserSession> {
      try {
        return this.validateBrowserSession(
          await this.send<BrowserSession>(
            "auth/browser/refresh", {}, false, "", true,
          ),
        );
      } catch (error) {
        if (error instanceof ApiError && error.status === 401)
          await this.completeCookieLogout(this.markCookieLogoutPending());
        throw error;
      }
    }

    private async readCookieSession(): Promise<BrowserSession> {
      let reply: BrowserSession;
      try {
        reply = this.validateBrowserSession(
          await this.send<BrowserSession>(
            "auth/browser/session", {}, false, "", true,
          ),
        );
      } catch (error) {
        if (!(error instanceof ApiError) || error.status !== 401)
          throw error;
        return this.requestCookieRefresh();
      }
      if (reply.expires_at - Date.now() / 1000 <= 60)
        return this.requestCookieRefresh();
      return reply;
    }

    private async logoutCookieSession(): Promise<void> {
      if (this.logoutInFlight) return this.logoutInFlight;
      const marker = this.markCookieLogoutPending();
      this.loggingOut = true;
      this.resetMemory(true, true);
      const pending = this.withSessionLock(async () => {
        if (this.readPendingCookieLogout() !== marker)
          throw new ApiError(
            "Sign-out could not be confirmed because another browser session changed while it was waiting. Review the current session before retrying.",
            "logout_unconfirmed",
            503,
          );
        await this.completeCookieLogout(marker);
      });
      this.logoutInFlight = pending;
      try {
        await pending;
      } finally {
        this.logoutInFlight = null;
        this.loggingOut = false;
      }
    }

    private markCookieLogoutPending(): string {
      if (this.pendingCookieLogout())
        throw new ApiError(
          "Another sign-out is pending. Retry after it completes.",
          "logout_unconfirmed",
          503,
        );
      const marker = `${Date.now()}:${crypto.randomUUID()}`;
      try {
        localStorage.setItem(this.cookiePendingLogoutKey, marker);
        if (localStorage.getItem(this.cookiePendingLogoutKey) !== marker)
          throw new Error("Browser storage did not retain the logout marker.");
      } catch {
        throw this.cookieStorageError();
      }
      return marker;
    }

    private pendingCookieLogout(): boolean {
      return this.readPendingCookieLogout() !== null;
    }

    private readPendingCookieLogout(): string | null {
      try {
        return localStorage.getItem(this.cookiePendingLogoutKey);
      } catch {
        throw this.cookieStorageError();
      }
    }

    private async finishPendingCookieLogout(): Promise<boolean> {
      const marker = this.readPendingCookieLogout();
      if (marker === null) return false;
      await this.completeCookieLogout(marker);
      return true;
    }

    private assertCookieStorageAvailable(): void {
      const probeKey = `${this.cookiePendingLogoutKey}.probe`;
      const probe = crypto.randomUUID();
      try {
        localStorage.setItem(probeKey, probe);
        if (localStorage.getItem(probeKey) !== probe)
          throw new Error("Browser storage did not retain the logout marker.");
        localStorage.removeItem(probeKey);
      } catch {
        throw this.cookieStorageError();
      }
    }

    private cookieStorageError(): ApiError {
      return new ApiError(
        "Secure browser sign-in requires writable storage to recover an unconfirmed sign-out after reload.",
        "storage_unavailable",
        503,
      );
    }

    private async completeCookieLogout(marker: string): Promise<void> {
      try {
        await this.send("auth/browser/logout", {}, false, "", true);
      } catch (error) {
        if (
          !(error instanceof ApiError) || error.status !== 401 ||
          error.code !== "session_missing"
        )
          throw new ApiError(
            "Could not confirm sign-out. The browser may still hold session cookies; retry when the host is reachable.",
            "logout_unconfirmed",
            503,
          );
      }
      try {
        if (localStorage.getItem(this.cookiePendingLogoutKey) === marker) {
          localStorage.removeItem(this.cookiePendingLogoutKey);
          if (localStorage.getItem(this.cookiePendingLogoutKey) === marker)
            throw new Error("Browser storage retained the logout marker.");
        }
      } catch {
        throw this.cookieStorageError();
      }
      try {
        localStorage.setItem(
          this.cookieLogoutKey,
          `${Date.now()}:${crypto.randomUUID()}`,
        );
      } catch {
        // Confirmed server revocation is sufficient if peer signal fails.
      }
    }

    private async refreshOrClear(
      expectedGeneration = this.authGeneration,
      expectedToken = this.refreshToken,
      expectedPersistenceGeneration = this.persistenceGeneration,
    ): Promise<void> {
      try {
        await this.refreshSession();
      } catch (error) {
        if (
          !this.loggingOut &&
          this.refreshToken &&
          expectedGeneration === this.authGeneration &&
          expectedToken === this.refreshToken &&
          expectedPersistenceGeneration === this.persistenceGeneration
        ) {
          await this.clear();
          this.onAuthenticationLost?.();
        }
        throw error;
      }
    }

    private async refreshSession(): Promise<void> {
      if (this.refreshInFlight) return this.refreshInFlight;
      if (!this.refreshToken)
        throw new ApiError("Sign in again to continue.", "unauthorized", 401);

      const generation = this.authGeneration;
      const persistenceGeneration = this.persistenceGeneration;
      const token = this.refreshToken;
      const suspendGeneration = this.authGeneration;
      const pending = this.remembered
        ? this.refreshRememberedSession(
            generation,
            persistenceGeneration,
            suspendGeneration,
            token,
          )
        : this.refreshMemorySession(generation, suspendGeneration, token);
      this.refreshInFlight = pending;
      try {
        await pending;
      } finally {
        if (this.refreshInFlight === pending) this.refreshInFlight = null;
      }
    }

    private async refreshMemorySession(
      generation: number,
      suspendGeneration: number,
      token: string,
    ): Promise<void> {
      const reply = await this.send<SessionReply>(
        "auth/refresh",
        { refresh_token: token },
        false,
        "",
      );
      const validated = this.validateSession(reply);
      if (generation !== this.authGeneration || token !== this.refreshToken) {
        void this.revokeSession({ refresh_token: validated.refresh_token });
        return;
      }
      if (suspendGeneration !== this.authGeneration) return;
      this.applyValidatedSession(validated, false);
    }

    private async refreshRememberedSession(
      generation: number,
      persistenceGeneration: number,
      suspendGeneration: number,
      token: string,
    ): Promise<void> {
      const locks = this.webLocks();
      if (!locks) throw this.rememberedRefreshLockError();

      await locks.request(
        this.refreshLockName,
        { mode: "exclusive" },
        async () => {
          if (
            generation !== this.authGeneration ||
            persistenceGeneration !== this.persistenceGeneration
          )
            return;

          const stored = this.readStoredSession();
          if (!stored) {
            throw new ApiError(
              "Sign in again to continue.",
              "unauthorized",
              401,
            );
          }
          if (
            stored.refresh_token !== token ||
            stored.bearer !== this.bearer
          ) {
            this.installStoredSession(stored);
            return;
          }

          const refreshingRecord: StoredSession = {
            ...stored,
            refreshing: true,
          };
          this.writeStoredSession(refreshingRecord);

          let reply: SessionReply;
          try {
            reply = await this.send<SessionReply>(
              "auth/refresh",
              { refresh_token: token },
              false,
              "",
            );
          } catch (error) {
            throw error;
          }

          const validated = this.validateSession(reply);
          const currentRaw = this.readStoredSessionRaw();
          const expectedRaw = JSON.stringify(refreshingRecord);
          if (
            currentRaw !== expectedRaw ||
            persistenceGeneration !== this.persistenceGeneration
          ) {
            void this.revokeSession({ refresh_token: validated.refresh_token });
            const latest = currentRaw
              ? this.parseStoredSession(currentRaw, false)
              : null;
            if (
              latest &&
              generation === this.authGeneration &&
              persistenceGeneration === this.persistenceGeneration
            ) {
              this.installStoredSession(latest);
              return;
            }
            if (generation === this.authGeneration) {
              throw new ApiError(
                "Sign in again to continue.",
                "unauthorized",
                401,
              );
            }
            return;
          }

          const rotatedRecord = this.storedSession(validated, stored.session_id);
          this.writeStoredSession(rotatedRecord);
          if (
            generation === this.authGeneration &&
            suspendGeneration === this.authGeneration &&
            persistenceGeneration === this.persistenceGeneration
          ) {
            this.applyValidatedSession(validated, true, stored.session_id);
          }
        },
      );
    }

    private async installSession(
      reply: SessionReply,
      generation: number,
      newSession: boolean,
      rememberRequested: boolean,
    ): Promise<void> {
      if (generation !== this.authGeneration)
        throw new Error("The sign-in attempt was cancelled.");
      const validated = this.validateSession(reply);
      const shouldRemember = rememberRequested && validated.remember_me;
      const previousSessionId = this.remembered ? this.sessionId : null;
      let sessionId: string | null = null;

      if (newSession) {
        try {
          if (shouldRemember) {
            const locks = this.webLocks();
            if (!locks) throw this.rememberedRefreshLockError();
            const stored = this.storedSession(validated);
            await locks.request(
              this.refreshLockName,
              { mode: "exclusive" },
              () => {
                if (generation !== this.authGeneration)
                  throw new Error("The sign-in attempt was cancelled.");
                this.writeStoredSession(stored);
              },
            );
            sessionId = stored.session_id;
          } else {
            await this.removeStoredSessionForIdentity(previousSessionId);
          }
        } catch (error) {
          if (
            generation === this.authGeneration &&
            (this.remembered ? this.sessionId : null) === previousSessionId
          )
            await this.clear();
          if (shouldRemember)
            void this.revokeSession({ refresh_token: validated.refresh_token });
          throw error;
        }
      }

      if (generation !== this.authGeneration)
        throw new Error("The sign-in attempt was cancelled.");
      this.applyValidatedSession(validated, shouldRemember, sessionId);
      if (newSession) {
        this.authGeneration += 1;
        this.persistenceGeneration += 1;
      }
    }

    private validateSession(reply: SessionReply): ValidatedSession {
      if (
        !/^ama_[a-f0-9]{64}$/.test(reply.bearer) ||
        !/^ama_[a-f0-9]{64}$/.test(reply.refresh_token) ||
        typeof reply.subject !== "string" ||
        !Array.isArray(reply.scopes) ||
        !reply.scopes.every((scope) => typeof scope === "string") ||
        !Number.isSafeInteger(reply.expires_at) ||
        !Number.isSafeInteger(reply.refresh_expires_at) ||
        reply.expires_at <= 0 ||
        reply.refresh_expires_at < reply.expires_at ||
        typeof reply.remember_me !== "boolean"
      ) {
        throw new ApiError(
          "The host returned an invalid authentication response.",
          "invalid_response",
          502,
        );
      }
      return {
        bearer: reply.bearer,
        refresh_token: reply.refresh_token,
        subject: reply.subject,
        role: ["administrator", "operator", "viewer"].includes(String(reply.role)) ? reply.role! : null,
        scopes: [...reply.scopes],
        expires_at: reply.expires_at,
        refresh_expires_at: reply.refresh_expires_at,
        remember_me: reply.remember_me,
      };
    }

    private applyValidatedSession(
      session: ValidatedSession,
      remembered: boolean,
      sessionId: string | null = null,
    ): void {
      this.bearer = session.bearer;
      this.refreshToken = session.refresh_token;
      this.accessExpiresAt = session.expires_at;
      this.subject = this.loggingOut ? "" : session.subject;
      this.role = this.loggingOut ? null : session.role ?? null;
      this.scopes = this.loggingOut ? null : [...session.scopes];
      this.remembered = remembered;
      this.sessionId = remembered ? sessionId : null;
      this.tokenGeneration += 1;
    }

    private installStoredSession(session: StoredSession): void {
      this.applyValidatedSession(session, true, session.session_id);
    }

    private storedSession(
      session: ValidatedSession,
      sessionId = this.newSessionId(),
    ): StoredSession {
      return {
        ...session,
        remember_me: true,
        version: SESSION_STORAGE_VERSION,
        base: this.base,
        session_id: sessionId,
        refreshing: false,
      };
    }

    private newSessionId(): string {
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
      bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
      const hex = Array.from(bytes, (byte) =>
        byte.toString(16).padStart(2, "0"),
      ).join("");
      return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
    }

    private readStoredSession(): StoredSession | null {
      const raw = this.readStoredSessionRaw();
      if (raw === null) return null;
      const stored = this.parseStoredSession(raw, false);
      if (stored) return stored;
      this.removeStoredSessionRequired(null, raw);
      return null;
    }

    private readStoredSessionRaw(): string | null {
      const storage = this.storage(false);
      if (!storage) return null;
      try {
        return storage.getItem(this.storageKey);
      } catch {
        throw this.storageError();
      }
    }

    private parseStoredSession(
      raw: string,
      allowRefreshing: boolean,
    ): StoredSession | null {
      let value: unknown;
      try {
        value = JSON.parse(raw);
      } catch {
        return null;
      }
      if (!value || typeof value !== "object") return null;
      const record = value as Partial<StoredSession>;
      if (
        record.version !== SESSION_STORAGE_VERSION ||
        record.base !== this.base ||
        typeof record.session_id !== "string" ||
        !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(
          record.session_id,
        ) ||
        record.remember_me !== true ||
        (record.refreshing !== false &&
          !(allowRefreshing && record.refreshing === true)) ||
        typeof record.bearer !== "string" ||
        !/^ama_[a-f0-9]{64}$/.test(record.bearer) ||
        typeof record.refresh_token !== "string" ||
        !/^ama_[a-f0-9]{64}$/.test(record.refresh_token) ||
        typeof record.subject !== "string" ||
        !Array.isArray(record.scopes) ||
        !record.scopes.every((scope) => typeof scope === "string") ||
        !Number.isSafeInteger(record.expires_at) ||
        !Number.isSafeInteger(record.refresh_expires_at) ||
        (record.expires_at as number) <= 0 ||
        (record.refresh_expires_at as number) <
          (record.expires_at as number) ||
        (record.refresh_expires_at as number) <= Date.now() / 1000
      ) {
        return null;
      }
      return record as StoredSession;
    }

    private writeStoredSession(session: StoredSession): void {
      const storage = this.storage(true);
      if (!storage) throw this.storageError();
      try {
        storage.setItem(this.storageKey, JSON.stringify(session));
      } catch {
        throw this.storageError();
      }
    }

    private removeStoredSessionRequired(
      expectedSessionId?: string | null,
      expectedRaw?: string,
    ): void {
      const storage = this.storage(false);
      if (!storage) return;
      try {
        if (expectedSessionId !== undefined) {
          const currentRaw = storage.getItem(this.storageKey);
          if (
            currentRaw === null ||
            !this.canRemoveStoredSession(
              currentRaw,
              expectedSessionId,
              expectedRaw,
            )
          )
            return;
        }
        storage.removeItem(this.storageKey);
      } catch {
        try {
          if (expectedSessionId !== undefined) {
            const currentRaw = storage.getItem(this.storageKey);
            if (
              currentRaw === null ||
              !this.canRemoveStoredSession(
                currentRaw,
                expectedSessionId,
                expectedRaw,
              )
            )
              return;
          }
          // If removal is blocked but writes still work, invalidate without
          // leaving either credential in browser storage.
          storage.setItem(this.storageKey, "{}");
        } catch {
          throw this.storageError();
        }
      }
    }

    private removeStoredSessionBestEffort(
      expectedSessionId?: string | null,
      expectedRaw?: string,
    ): void {
      let storage: Storage | null;
      try {
        storage = this.storage(false);
      } catch {
        return;
      }
      if (!storage) return;
      try {
        if (expectedSessionId !== undefined) {
          const currentRaw = storage.getItem(this.storageKey);
          if (
            currentRaw === null ||
            !this.canRemoveStoredSession(
              currentRaw,
              expectedSessionId,
              expectedRaw,
            )
          )
            return;
        }
        storage.removeItem(this.storageKey);
      } catch {
        try {
          if (expectedSessionId !== undefined) {
            const currentRaw = storage.getItem(this.storageKey);
            if (
              currentRaw === null ||
              !this.canRemoveStoredSession(
                currentRaw,
                expectedSessionId,
                expectedRaw,
              )
            )
              return;
          }
          storage.setItem(this.storageKey, "{}");
        } catch {
          // Memory is already cleared. Storage access is unavailable.
        }
      }
    }

    private async removeStoredSessionForIdentity(
      expectedSessionId: string | null,
      expectedRaw?: string,
    ): Promise<void> {
      const removeCurrentSession = (): void => {
        try {
          const raw = this.readStoredSessionRaw();
          if (raw === null || (expectedRaw !== undefined && raw !== expectedRaw))
            return;
          const current = this.parseStoredSession(raw, true);
          if (
            current &&
            (expectedRaw !== undefined ||
              expectedSessionId === null ||
              current.session_id !== expectedSessionId)
          )
            return;
          this.removeStoredSessionRequired(expectedSessionId, raw);
        } catch {
          // Local sign-out and nonremembered login remain available when storage fails.
        }
      };

      try {
        await this.withSessionLock(removeCurrentSession);
      } catch {
        if (!this.webLocks()) removeCurrentSession();
      }
    }

    private readStoredSessionForCleanup(
      tokens: Set<string>,
      expectedSessionId: string | null,
    ): StoredSession | null {
      try {
        const raw = this.readStoredSessionRaw();
        if (raw === null) return null;
        const stored = this.parseStoredSession(raw, true);
        if (
          stored &&
          expectedSessionId !== null &&
          stored.session_id === expectedSessionId
        ) {
          tokens.add(stored.refresh_token);
          return stored;
        }
      } catch {
        // Sign-out still clears memory when storage is inaccessible.
      }
      return null;
    }

    private canRemoveStoredSession(
      raw: string,
      expectedSessionId: string | null | undefined,
      expectedRaw?: string,
    ): boolean {
      if (expectedRaw !== undefined) return raw === expectedRaw;
      if (expectedSessionId === undefined) return true;
      const stored = this.parseStoredSession(raw, true);
      if (!stored) return true;
      return (
        expectedSessionId !== null &&
        stored.base === this.base &&
        stored.session_id === expectedSessionId
      );
    }

    private async revokeSession(payload: Payload): Promise<void> {
      try {
        await this.send("auth/logout", payload, false, "");
      } catch {
        // Local sign-out is immediate even when revocation cannot be confirmed.
      }
    }

    private webLocks(): LockManager | null {
      if (typeof navigator === "undefined") return null;
      try {
        return navigator.locks ?? null;
      } catch {
        return null;
      }
    }

    private async withSessionLock<T>(action: () => T | Promise<T>): Promise<T> {
      const locks = this.webLocks();
      if (!locks) return action();
      return locks.request(
        this.refreshLockName,
        { mode: "exclusive" },
        action,
      );
    }

    private rememberedRefreshLockError(): ApiError {
      return new ApiError(
        "Remember me requires Web Locks in a secure browser context. Sign in without Remember me when unavailable.",
        "refresh_lock_unavailable",
        503,
      );
    }

    private storage(required: boolean): Storage | null {
      if (typeof localStorage === "undefined") {
        if (required) throw this.storageError();
        return null;
      }
      try {
        return localStorage;
      } catch {
        throw this.storageAccessError();
      }
    }

    private storageError(): ApiError {
      return new ApiError(
        "Browser storage is unavailable. Sign in again when storage is enabled.",
        "storage_unavailable",
        503,
      );
    }

    private storageAccessError(): ApiError {
      return new ApiError(
        "Browser storage is unavailable. Sign in again when storage is enabled.",
        "storage_access_unavailable",
        503,
      );
    }

    private handleStorageChange(event: StorageEvent): void {
      if (this.loggingOut) return;
      if (
        event.key === this.cookiePendingLogoutKey &&
        event.newValue !== null && this.cookieSession
      ) {
        let stillPending = true;
        try {
          stillPending =
            localStorage.getItem(this.cookiePendingLogoutKey) === event.newValue;
        } catch {
          // Storage denial means the peer's pending sign-out is unobservable.
        }
        if (stillPending) {
          this.resetMemory(true);
          this.onAuthenticationLost?.();
        } else {
          void this.checkCookieSessionAfterPeerLogout();
        }
        return;
      }
      if (event.key === this.cookieLogoutKey && this.cookieSession) {
        void this.checkCookieSessionAfterPeerLogout();
        return;
      }
      // Storage.clear() broadcasts a null key. Re-read our record below so a
      // peer tab cannot keep using a remembered bearer after that clear.
      if (event.key !== null && event.key !== this.storageKey) return;
      if (this.usesSecureCookies) return;
      let currentRaw: string | null;
      try {
        currentRaw = this.readStoredSessionRaw();
      } catch {
        if (this.remembered) {
          this.resetMemory(true);
          this.onAuthenticationLost?.();
        }
        return;
      }
      if (currentRaw === null) {
        if (this.remembered) {
          this.resetMemory(true);
          this.onAuthenticationLost?.();
        }
        return;
      }

      const stored = this.parseStoredSession(currentRaw, true);
      if (!stored) {
        if (this.remembered) {
          this.resetMemory(true);
          this.onAuthenticationLost?.();
        }
        void this.removeStoredSessionForIdentity(null, currentRaw);
        return;
      }
      if (stored.refreshing) return;
      if (this.remembered) this.installStoredSession(stored);
    }

    private async checkCookieSessionAfterPeerLogout(): Promise<void> {
      const generation = this.authGeneration;
      try {
        await this.withSessionLock(async () => {
          if (!this.cookieSession || generation !== this.authGeneration) return;
          try {
            const reply = await this.send<BrowserSession>(
              "auth/browser/session", {}, false, "", true,
            );
            if (generation === this.authGeneration)
              this.applyBrowserSession(reply);
          } catch (error) {
            if (
              !(error instanceof ApiError) ||
              (error.status !== 401 && error.code !== "invalid_response")
            )
              return;
            if (generation === this.authGeneration) {
              this.resetMemory(true);
              this.onAuthenticationLost?.();
            }
          }
        });
      } catch {
        // A failed peer check is retried by the next protected request.
      }
    }

    private resetMemory(
      invalidatePersistence: boolean,
      preserveRefresh = false,
    ): void {
      this.authGeneration += 1;
      this.tokenGeneration += 1;
      if (invalidatePersistence) this.persistenceGeneration += 1;
      this.bearer = "";
      this.refreshToken = "";
      this.accessExpiresAt = 0;
      this.subject = "";
      this.role = null;
      this.scopes = null;
      this.remembered = false;
      this.cookieSession = false;
      this.cookieRefreshInFlight = null;
      this.sessionId = null;
      this.refreshInFlight = null;
      this.abortRequests(preserveRefresh);
    }

    private abortRequests(preserveRefresh = false): void {
      for (const request of this.requests) {
        if (!preserveRefresh || !this.refreshRequests.has(request)) {
          request.abort();
          this.requests.delete(request);
          this.refreshRequests.delete(request);
        }
      }
    }

    private async send<T>(
      route: string,
      payload: Payload,
      protectedRoute: boolean,
      bearer: string,
      cookieAuth = false,
    ): Promise<T> {
      const controller = new AbortController();
      const timer = window.setTimeout(() => controller.abort(), 12_000);
      this.requests.add(controller);
      if (route === "auth/refresh" || route === "auth/browser/refresh")
        this.refreshRequests.add(controller);
      try {
        const headers: Record<string, string> = {
          "Content-Type": "application/json",
        };
        if (cookieAuth) headers["X-Amaterasu-CSRF"] = "1";
        else if (protectedRoute) headers.Authorization = `Bearer ${bearer}`;
        const response = await fetch(`${this.base}/v1/${route}`, {
          method: "POST",
          headers,
          body: JSON.stringify(payload),
          signal: controller.signal,
          credentials: cookieAuth ? "same-origin" : "omit",
          cache: "no-store",
          referrerPolicy: "no-referrer",
        });
        const body = (await response.json()) as Payload;
        if (!response.ok || body.ok !== true)
          throw this.failure(body, response.status);
        return body as T;
      } catch (error) {
        if (error instanceof ApiError) throw error;
        if (controller.signal.aborted)
          throw new Error(
            "The request timed out or was cancelled. A change may have completed; refresh before retrying.",
          );
        throw new Error(
          "Could not reach the host. Check its address, connection, and allowed browser origin.",
        );
      } finally {
        clearTimeout(timer);
        this.requests.delete(controller);
        this.refreshRequests.delete(controller);
      }
    }

    private failure(body: Payload, status: number): ApiError {
      const detail =
        typeof body.error === "object" && body.error
          ? (body.error as Payload)
          : body;
      const code =
        typeof detail.code === "string" ? detail.code : "request_failed";
      const messages: Record<string, string> = {
        unauthorized:
          "Access denied. Sign in again or use a token with the required scope.",
        forbidden: "This account cannot perform that action.",
        rate_limited:
          "Too many sign-in attempts. Wait a minute before trying again.",
        revision_conflict:
          "This data changed elsewhere. Refresh before saving again.",
        not_registered:
          "No settings schema has been registered for this console.",
      };
      return new ApiError(
        messages[code] ??
          (typeof detail.message === "string"
            ? detail.message
            : code.replaceAll("_", " ")),
        code,
        status,
      );
    }
  }
}
