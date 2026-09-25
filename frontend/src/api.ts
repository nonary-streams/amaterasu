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
    private requests = new Set<AbortController>();
    private readonly base: string;
    private readonly storageKey: string;
    private readonly refreshLockName: string;
    private remembered = false;
    onAuthenticationLost: (() => void) | null = null;
    subject = "";
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
      this.storageKey = `amaterasu.session.${encodeURIComponent(this.base)}`;
      this.refreshLockName = `amaterasu.session.refresh:${this.base}`;
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
      const generation = this.authGeneration;
      const reply = await this.post<SessionReply>(
        "auth/login",
        { username, password, remember_me: rememberMe },
        false,
      );
      this.installSession(reply, generation, true, rememberMe);
    }

    async restoreSession(): Promise<boolean> {
      this.resetMemory(true);
      const stored = this.readStoredSession();
      if (!stored) return false;

      this.installStoredSession(stored);
      const generation = this.authGeneration;
      const token = this.refreshToken;
      const persistenceGeneration = this.persistenceGeneration;
      if (stored.expires_at - Date.now() / 1000 <= 60)
        await this.refreshOrClear(generation, token, persistenceGeneration);
      return generation === this.authGeneration && this.bearer !== "";
    }

    suspend(): void {
      // Keep a pending marker if a remembered refresh was in flight. A reload
      // then clears it rather than replaying a refresh token that may be spent.
      this.resetMemory(false);
    }

    useToken(token: string): void {
      if (!/^ama_[a-f0-9]{64}$/.test(token))
        throw new Error(
          "Enter an Amaterasu token beginning with ama_ followed by 64 hexadecimal characters.",
        );
      this.clear();
      this.bearer = token;
      this.subject = "API token";
      this.scopes = null;
      this.tokenGeneration += 1;
    }

    async logout(): Promise<void> {
      const refreshTokens = new Set<string>();
      const bearerToken = this.refreshToken ? "" : this.bearer;
      if (this.refreshToken) refreshTokens.add(this.refreshToken);
      const storedBeforeLogout = this.readStoredSessionForCleanup(refreshTokens);

      this.resetMemory(true);
      this.removeStoredSessionBestEffort();

      const locks = this.webLocks();
      if (locks) {
        try {
          await locks.request(
            this.refreshLockName,
            { mode: "exclusive" },
            () => {
              // A refresh already holding the lock may have rotated before the
              // immediate removal. Revoke its latest pair if it left one.
              const latest = this.readStoredSessionForCleanup(new Set<string>());
              if (
                latest &&
                storedBeforeLogout &&
                this.sameSessionIdentity(latest, storedBeforeLogout)
              ) {
                refreshTokens.add(latest.refresh_token);
                this.removeStoredSessionBestEffort();
              }
            },
          );
        } catch {
          // Local sign-out is immediate even when lock cleanup cannot complete.
        }
      }

      for (const refreshToken of refreshTokens) {
        await this.revokeSession({ refresh_token: refreshToken });
      }
      if (bearerToken) await this.revokeSession({ bearer: bearerToken });
    }

    clear(): void {
      const staleRefreshTokens = new Set<string>();
      if (this.refreshToken) staleRefreshTokens.add(this.refreshToken);
      const storedBeforeClear = this.readStoredSessionForCleanup(staleRefreshTokens);
      this.resetMemory(true);
      this.removeStoredSessionBestEffort();
      this.scheduleStoredSessionRemoval(staleRefreshTokens, storedBeforeClear);
    }

    allows(scope: string): boolean {
      return this.scopes === null || this.scopes.includes(scope);
    }

    async post<T>(
      route: string,
      payload: Payload = {},
      protectedRoute = true,
    ): Promise<T> {
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
        return await this.send<T>(route, payload, protectedRoute, bearerUsed);
      } catch (error) {
        if (
          !protectedRoute ||
          !(error instanceof ApiError) ||
          error.code !== "unauthorized" ||
          !this.refreshToken ||
          authGeneration !== this.authGeneration
        ) {
          throw error;
        }

        // A concurrent request or tab may already have rotated this bearer.
        // Reuse its pair instead of replaying a stale refresh token.
        if (tokenGeneration === this.tokenGeneration) await this.refreshOrClear();
        if (authGeneration !== this.authGeneration || !this.bearer) throw error;
        return this.send<T>(route, payload, true, this.bearer);
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
          this.refreshToken &&
          expectedGeneration === this.authGeneration &&
          expectedToken === this.refreshToken &&
          expectedPersistenceGeneration === this.persistenceGeneration
        ) {
          this.clear();
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
      if (!locks)
        throw new ApiError(
          "This browser cannot safely refresh a remembered session. Use a browser with Web Locks or sign in without Remember me.",
          "refresh_lock_unavailable",
          503,
        );

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
            this.applyValidatedSession(validated, true);
          }
        },
      );
    }

    private installSession(
      reply: SessionReply,
      generation: number,
      newSession: boolean,
      rememberRequested: boolean,
    ): void {
      if (generation !== this.authGeneration)
        throw new Error("The sign-in attempt was cancelled.");
      const validated = this.validateSession(reply);
      const shouldRemember = rememberRequested && validated.remember_me;

      if (newSession) {
        try {
          if (shouldRemember) {
            this.writeStoredSession(this.storedSession(validated));
          } else {
            try {
              const storage = this.storage(false);
              if (storage) {
                const staleRefreshTokens = new Set<string>();
                const storedBeforeClear = this.readStoredSessionForCleanup(
                  staleRefreshTokens,
                );
                this.removeStoredSessionRequired();
                this.scheduleStoredSessionRemoval(
                  staleRefreshTokens,
                  storedBeforeClear,
                );
              }
            } catch (error) {
              if (
                !(error instanceof ApiError) ||
                error.code !== "storage_access_unavailable"
              )
                throw error;
            }
          }
        } catch (error) {
          this.clear();
          if (shouldRemember)
            void this.revokeSession({ refresh_token: validated.refresh_token });
          throw error;
        }
      }

      if (generation !== this.authGeneration)
        throw new Error("The sign-in attempt was cancelled.");
      this.applyValidatedSession(validated, shouldRemember);
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
        scopes: [...reply.scopes],
        expires_at: reply.expires_at,
        refresh_expires_at: reply.refresh_expires_at,
        remember_me: reply.remember_me,
      };
    }

    private applyValidatedSession(
      session: ValidatedSession,
      remembered: boolean,
    ): void {
      this.bearer = session.bearer;
      this.refreshToken = session.refresh_token;
      this.accessExpiresAt = session.expires_at;
      this.subject = session.subject;
      this.scopes = [...session.scopes];
      this.remembered = remembered;
      this.tokenGeneration += 1;
    }

    private installStoredSession(session: StoredSession): void {
      this.applyValidatedSession(session, true);
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
      this.removeStoredSessionRequired();
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

    private removeStoredSessionRequired(): void {
      const storage = this.storage(false);
      if (!storage) return;
      try {
        storage.removeItem(this.storageKey);
      } catch {
        try {
          // If removal is blocked but writes still work, invalidate without
          // leaving either credential in browser storage.
          storage.setItem(this.storageKey, "{}");
        } catch {
          throw this.storageError();
        }
      }
    }

    private removeStoredSessionBestEffort(): void {
      let storage: Storage | null;
      try {
        storage = this.storage(false);
      } catch {
        return;
      }
      if (!storage) return;
      try {
        storage.removeItem(this.storageKey);
      } catch {
        try {
          storage.setItem(this.storageKey, "{}");
        } catch {
          // Memory is already cleared. Storage access is unavailable.
        }
      }
    }

    private scheduleStoredSessionRemoval(
      staleRefreshTokens: Set<string>,
      session: StoredSession | null,
    ): void {
      const locks = this.webLocks();
      if (!locks) return;
      void locks
        .request(this.refreshLockName, { mode: "exclusive" }, () => {
          try {
            const raw = this.readStoredSessionRaw();
            if (raw === null) return;
            const current = this.parseStoredSession(raw, true);
            if (
              !current ||
              staleRefreshTokens.has(current.refresh_token) ||
              (session !== null && this.sameSessionIdentity(current, session))
            )
              this.removeStoredSessionBestEffort();
          } catch {
            // The synchronous clear already removed what storage allowed.
          }
        })
        .catch(() => undefined);
    }

    private readStoredSessionForCleanup(
      tokens: Set<string>,
    ): StoredSession | null {
      try {
        const raw = this.readStoredSessionRaw();
        if (raw === null) return null;
        const stored = this.parseStoredSession(raw, true);
        if (stored) {
          tokens.add(stored.refresh_token);
          return stored;
        }
      } catch {
        // Sign-out still clears memory when storage is inaccessible.
      }
      return null;
    }

    private sameSessionIdentity(
      left: StoredSession,
      right: StoredSession,
    ): boolean {
      return (
        left.base === right.base && left.session_id === right.session_id
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
      if (event.key !== this.storageKey) return;
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
        try {
          if (this.readStoredSessionRaw() === currentRaw)
            this.removeStoredSessionBestEffort();
        } catch {
          // The invalid value will be rejected again on the next restore.
        }
        return;
      }
      if (stored.refreshing) return;
      if (this.remembered) this.installStoredSession(stored);
    }

    private resetMemory(invalidatePersistence: boolean): void {
      this.authGeneration += 1;
      this.tokenGeneration += 1;
      if (invalidatePersistence) this.persistenceGeneration += 1;
      this.bearer = "";
      this.refreshToken = "";
      this.accessExpiresAt = 0;
      this.subject = "";
      this.scopes = null;
      this.remembered = false;
      this.refreshInFlight = null;
      for (const request of this.requests) request.abort();
      this.requests.clear();
    }

    private async send<T>(
      route: string,
      payload: Payload,
      protectedRoute: boolean,
      bearer: string,
    ): Promise<T> {
      const controller = new AbortController();
      const timer = window.setTimeout(() => controller.abort(), 12_000);
      this.requests.add(controller);
      try {
        const headers: Record<string, string> = {
          "Content-Type": "application/json",
        };
        if (protectedRoute) headers.Authorization = `Bearer ${bearer}`;
        const response = await fetch(`${this.base}/v1/${route}`, {
          method: "POST",
          headers,
          body: JSON.stringify(payload),
          signal: controller.signal,
          credentials: "omit",
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
