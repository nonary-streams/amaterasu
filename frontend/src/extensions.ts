namespace Amaterasu {
  interface ExtensionMethod { alias: string }
  export interface ExtensionDescriptor {
    id: string;
    name: string;
    digest: string;
    sdk_version: number;
    entrypoint: string;
    methods: ExtensionMethod[];
  }
  interface ExtensionPage {
    title: string;
    mount(container: HTMLElement, sdk: ExtensionPageSdk): void | (() => void);
  }
  interface ExtensionPageSdk {
    readonly version: 1;
    readonly signal: AbortSignal;
    readonly ui: ExtensionUiV1;
    call(method: string, payload?: Payload): Promise<unknown>;
  }
  interface ExtensionRegistry {
    v1: { register(id: string, page: ExtensionPage): void };
  }
  const registrations = new Map<string, ExtensionPage>();
  const loads = new Map<string, Promise<ExtensionPage>>();
  const pendingScripts = new WeakMap<HTMLScriptElement, string>();
  const registry: ExtensionRegistry = {
    v1: {
      register(id, page) {
        const script = document.currentScript;
        if (!(script instanceof HTMLScriptElement) || pendingScripts.get(script)?.split("@")[0] !== id ||
            registrations.has(pendingScripts.get(script)!) || typeof page !== "object" || page === null ||
            typeof page.title !== "string" || !page.title.trim() || page.title.length > 128 ||
            typeof page.mount !== "function") throw new Error("Invalid extension registration.");
        registrations.set(pendingScripts.get(script)!, page);
      },
    },
  };
  Object.freeze(registry.v1);
  Object.freeze(registry);
  (window as unknown as { AmaterasuExtensions: ExtensionRegistry }).AmaterasuExtensions = registry;

  function validExtension(value: unknown): value is ExtensionDescriptor {
    if (!value || typeof value !== "object") return false;
    const record = value as Record<string, unknown>;
    return typeof record.id === "string" && /^[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?$/.test(record.id) && record.id.length <= 64 &&
      typeof record.name === "string" && !!record.name.trim() && record.name.length <= 128 &&
      typeof record.digest === "string" && /^[a-f0-9]{64}$/.test(record.digest) &&
      record.sdk_version === 1 && typeof record.entrypoint === "string" &&
      /^[a-zA-Z0-9._/-]+\.js$/.test(record.entrypoint) &&
      !record.entrypoint.split("/").some((part) => !part || part === "." || part === "..") &&
      Array.isArray(record.methods) && record.methods.length <= 64 &&
      record.methods.every((method: unknown) => !!method && typeof method === "object" &&
        typeof (method as Record<string, unknown>).alias === "string");
  }

  export class ExtensionPages {
    private descriptors = new Map<string, ExtensionDescriptor>();
    private cleanup: (() => void) | null = null;
    private controller: AbortController | null = null;
    private generation = 0;
    constructor(private readonly api: Api) {}

    async discover(): Promise<ExtensionDescriptor[]> {
      const generation = ++this.generation;
      const reply = await this.api.post<{ extensions: unknown }>("extensions/list");
      if (generation !== this.generation || !Array.isArray(reply.extensions)) return [];
      const entries = reply.extensions.filter(validExtension);
      const counts = new Map<string, number>();
      for (const entry of entries) counts.set(entry.id, (counts.get(entry.id) ?? 0) + 1);
      this.descriptors = new Map(entries.filter((entry) => counts.get(entry.id) === 1).map((entry) => [entry.id, entry]));
      return [...this.descriptors.values()];
    }

    dispose(): void {
      this.controller?.abort();
      this.controller = null;
      const cleanup = this.cleanup;
      this.cleanup = null;
      try { cleanup?.(); } catch (error) { console.error("Extension cleanup failed", error); }
    }

    reset(): void {
      this.generation++;
      this.dispose();
      this.descriptors.clear();
    }

    async mount(id: string, container: HTMLElement): Promise<void> {
      this.dispose();
      const descriptor = this.descriptors.get(id);
      if (!descriptor) throw new Error("Extension is unavailable.");
      const controller = new AbortController();
      this.controller = controller;
      const registrationKey = `${id}@${descriptor.digest}`;
      let page = registrations.get(registrationKey);
      if (!page) {
        let loading = loads.get(registrationKey);
        if (!loading) {
          const script = document.createElement("script");
          const base = document.querySelector<HTMLMetaElement>('meta[name="amaterasu-api-base"]')?.content ?? "";
          const path = descriptor.entrypoint.split("/").map(encodeURIComponent).join("/");
          script.src = `${base}/v1/extensions/${encodeURIComponent(id)}/${descriptor.digest}/assets/${path}`;
          pendingScripts.set(script, registrationKey);
          loading = new Promise<ExtensionPage>((resolve, reject) => {
            script.onload = () => {
              const registered = registrations.get(registrationKey);
              if (registered) resolve(registered);
              else reject(new Error("Extension did not register a page."));
            };
            script.onerror = () => reject(new Error("Extension could not be loaded."));
            document.head.append(script);
          }).finally(() => {
            pendingScripts.delete(script);
            script.remove();
            loads.delete(registrationKey);
          });
          loads.set(registrationKey, loading);
        }
        page = await loading;
      }
      if (controller.signal.aborted) return;
      if (!page) throw new Error("Extension did not register a page.");
      const aliases = new Set(descriptor.methods.map((method) => method.alias));
      const sdk: ExtensionPageSdk = Object.freeze({
        version: 1 as const,
        signal: controller.signal,
        ui: createExtensionUiV1(controller.signal),
        call: (method: string, payload: Payload = {}) => {
          if (controller.signal.aborted || !aliases.has(method)) return Promise.reject(new Error("Extension method is unavailable."));
          return this.api.post<{ result: unknown }>("extensions/call", { id, digest: descriptor.digest, method, payload })
            .then((reply) => {
              if (!Object.hasOwn(reply, "result")) throw new Error("Invalid extension response.");
              return reply.result;
            });
        },
      });
      const cleanup = page.mount(container, sdk);
      if (typeof cleanup === "function") this.cleanup = cleanup;
    }
  }
}
