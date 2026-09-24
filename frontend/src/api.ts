namespace Amaterasu {
  type SessionReply = LoginReply & {
    refresh_token: string;
    refresh_expires_at: number;
    remember_me: boolean;
  };

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
    private tokenGeneration = 0;
    private refreshInFlight: Promise<void> | null = null;
    private requests = new Set<AbortController>();
    private readonly base: string;
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
      this.installSession(reply, generation, true);
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
      const payload = this.refreshToken
        ? { refresh_token: this.refreshToken }
        : this.bearer
          ? { bearer: this.bearer }
          : null;
      this.clear();
      if (payload) {
        try {
          await this.post("auth/logout", payload, false);
        } catch {
          // Local sign-out is immediate even when revocation cannot be confirmed.
        }
      }
    }

    clear(): void {
      this.authGeneration += 1;
      this.tokenGeneration += 1;
      this.bearer = "";
      this.refreshToken = "";
      this.accessExpiresAt = 0;
      this.subject = "";
      this.scopes = null;
      this.refreshInFlight = null;
      for (const request of this.requests) request.abort();
      this.requests.clear();
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

        // A concurrent request may already have rotated this bearer. Retry with
        // that pair instead of rotating its refresh token a second time.
        if (tokenGeneration === this.tokenGeneration) await this.refreshOrClear();
        return this.send<T>(route, payload, true, this.bearer);
      }
    }

    private async refreshOrClear(): Promise<void> {
      try {
        await this.refreshSession();
      } catch (error) {
        if (this.refreshToken) {
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
      const token = this.refreshToken;
      const pending = (async () => {
        const reply = await this.send<SessionReply>(
          "auth/refresh",
          { refresh_token: token },
          false,
          "",
        );
        if (generation !== this.authGeneration || token !== this.refreshToken)
          throw new Error("The sign-in session was cleared.");
        this.installSession(reply, generation, false);
      })();
      this.refreshInFlight = pending;
      try {
        await pending;
      } finally {
        if (this.refreshInFlight === pending) this.refreshInFlight = null;
      }
    }

    private installSession(
      reply: SessionReply,
      generation: number,
      newSession: boolean,
    ): void {
      if (generation !== this.authGeneration)
        throw new Error("The sign-in attempt was cancelled.");
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
      this.bearer = reply.bearer;
      this.refreshToken = reply.refresh_token;
      this.accessExpiresAt = reply.expires_at;
      this.subject = reply.subject;
      this.scopes = reply.scopes;
      this.tokenGeneration += 1;
      if (newSession) this.authGeneration += 1;
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
