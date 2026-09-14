namespace Amaterasu {
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
    private requests = new Set<AbortController>();
    private readonly base: string;
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

    async login(username: string, password: string): Promise<void> {
      const reply = await this.post<LoginReply>(
        "auth/login",
        { username, password },
        false,
      );
      this.bearer = reply.bearer;
      this.subject = reply.subject;
      this.scopes = reply.scopes;
    }

    useToken(token: string): void {
      if (!/^ama_[a-f0-9]{64}$/.test(token))
        throw new Error(
          "Enter an Amaterasu token beginning with ama_ followed by 64 hexadecimal characters.",
        );
      this.bearer = token;
      this.subject = "API token";
      this.scopes = null;
    }

    clear(): void {
      this.bearer = "";
      this.subject = "";
      this.scopes = null;
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
      const controller = new AbortController();
      const timer = window.setTimeout(() => controller.abort(), 12_000);
      this.requests.add(controller);
      try {
        const headers: Record<string, string> = {
          "Content-Type": "application/json",
        };
        if (protectedRoute) headers.Authorization = `Bearer ${this.bearer}`;
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
