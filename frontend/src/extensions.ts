namespace Amaterasu {
  interface ExtensionMethod { alias: string }
  export interface ExtensionDescriptor {
    id: string;
    name: string;
    digest: string;
    sdk_version: number;
    entrypoint: string;
    placements?: ("page" | "application" | "overview" | "settings")[];
    settings_provider?: string | null;
    settings_owner?: string | null;
    methods: ExtensionMethod[];
  }
  interface ExtensionPage {
    title: string;
    mount?(container: HTMLElement, sdk: ExtensionPageSdk): void | (() => void);
    mountOverview?(container: HTMLElement, sdk: ExtensionPageSdk): void | (() => void);
    mountSettings?(container: HTMLElement, sdk: ExtensionPageSdk): void | (() => void);
    mountApplication?(container: HTMLElement, sdk: ExtensionPageSdk): void | (() => void);
  }
  export interface ApplicationExtensionContext { readonly id: string; readonly name: string; readonly onDocumentChange?: () => void }
  export interface ApplicationSettingsDocument {
    revision: number;
    values: Record<string, unknown>;
    effective: Record<string, { value: unknown; source: unknown }>;
  }
  export interface ApplicationSettingsApi {
    readonly version: 1;
    readonly provider: string;
    readonly canWrite: boolean;
    readonly scope?: ExtensionSettingsScope;
    describe?(): Promise<ExtensionSettingsDescriptor>;
    read(): Promise<ApplicationSettingsDocument>;
    update(expectedRevision: number, values: Payload): Promise<{ revision: number }>;
  }
  export type ExtensionSettingsScope = { readonly kind: "host" } | { readonly kind: "application"; readonly id: string };
  export interface ExtensionSettingsDescriptor {
    provider: string;
    display_name?: string | null;
    options: {
      id: string; label: string; help: string; definition: ExtensionFieldDefinition;
      choice_labels?: Record<string,string>; presentation?: string | null;
      allowed_scopes: string[]; apply_policy: string;
    }[];
  }
  export interface ApplicationExtensionMount {
    readonly ready: Promise<void>;
    dispose(): void;
    isDirty(): boolean;
    isBusy(): boolean;
  }
  export interface ExtensionPageSdk {
    readonly version: 1;
    readonly signal: AbortSignal;
    readonly features?: readonly string[];
    readonly ui?: ExtensionUiWithDialogsV1 & ExtensionUiTabsV1 & ExtensionUiChartsV1;
    readonly assets?: { stylesheet(path: string): Promise<void> };
    canCall?(method: string): boolean;
    readonly application?: Pick<ApplicationExtensionContext, "id" | "name"> & { readonly notifyChanged?: () => void };
    readonly settings?: ApplicationSettingsApi;
    setDirty?(dirty: boolean): void;
    setBusy?(busy: boolean): void;
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
            (typeof page.mount !== "function" && typeof page.mountApplication !== "function" && typeof page.mountOverview !== "function" && typeof page.mountSettings !== "function") ||
            (page.mount !== undefined && typeof page.mount !== "function") ||
            (page.mountApplication !== undefined && typeof page.mountApplication !== "function") ||
            (page.mountOverview !== undefined && typeof page.mountOverview !== "function") ||
            (page.mountSettings !== undefined && typeof page.mountSettings !== "function")) throw new Error("Invalid extension registration.");
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
      (record.placements === undefined || (Array.isArray(record.placements) && record.placements.length > 0 && record.placements.length <= 4 &&
        new Set(record.placements).size === record.placements.length && record.placements.every((place: unknown) => place === "page" || place === "application" || place === "overview" || place === "settings"))) &&
      (record.settings_provider == null || (typeof record.settings_provider === "string" && /^[a-z0-9._-]{1,128}$/.test(record.settings_provider) &&
        Array.isArray(record.placements) && (record.placements.includes("application") || record.placements.includes("settings")))) &&
      (record.settings_owner == null || (typeof record.settings_owner === "string" && /^[a-z0-9._-]{1,128}$/.test(record.settings_owner) &&
        Array.isArray(record.placements) && (record.placements.includes("application") || record.placements.includes("settings")))) &&
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
    private dirty = false;
    private busy = false;
    isDirty(): boolean { return this.dirty || this.busy; }
    isBusy(): boolean { return this.busy; }
    private applicationMounts = new Set<ApplicationExtensionMount>();
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
      for (const mount of this.applicationMounts) mount.dispose();
      this.applicationMounts.clear();
      this.controller?.abort();
      this.controller = null;
      this.dirty = false; this.busy = false;
      const cleanup = this.cleanup;
      this.cleanup = null;
      try { cleanup?.(); } catch (error) { console.error("Extension cleanup failed", error); }
    }

    reset(): void {
      this.generation++;
      this.dispose();
      this.descriptors.clear();
    }

    mountApplication(id: string, container: HTMLElement, application: ApplicationExtensionContext): ApplicationExtensionMount {
      if (typeof application.id !== "string" || !/^app-[0-9a-f]{16}$/.test(application.id) ||
          application.id === "app-0000000000000000" || typeof application.name !== "string" ||
          !application.name || application.name.length > 512 || /[\u0000-\u001f\u007f-\u009f]/u.test(application.name))
        throw new Error("Invalid application context.");
      const child = new ExtensionPages(this.api);
      child.descriptors = new Map(this.descriptors);
      let dirty = false;
      let busy = false;
      const state = (kind: "dirty" | "busy", value: boolean) => {
        if (typeof value !== "boolean") throw new Error("Invalid application section state.");
        if (kind === "dirty") dirty = value;
        else busy = value;
      };
      const mount: ApplicationExtensionMount = {
        ready: child.mount(id, container, application, state),
        dispose: () => { child.dispose(); dirty = false; busy = false; this.applicationMounts.delete(mount); },
        isDirty: () => dirty || busy,
        isBusy: () => busy,
      };
      this.applicationMounts.add(mount);
      return mount;
    }

    async mount(id: string, container: HTMLElement, application?: ApplicationExtensionContext,
      state?: (kind: "dirty" | "busy", value: boolean) => void, slot?: "overview" | "settings"): Promise<void> {
      this.dispose();
      const descriptor = this.descriptors.get(id);
      if (!descriptor) throw new Error("Extension is unavailable.");
      const placement = slot ?? (application ? "application" : "page");
      if (!(descriptor.placements ?? ["page"]).includes(placement)) throw new Error("Extension is unavailable in this location.");
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
      if (controller.signal.aborted || container.isConnected === false) return;
      if (!page) throw new Error("Extension did not register a page.");
      const aliases = new Set(descriptor.methods.map((method) => method.alias));
      const sdk: ExtensionPageSdk = Object.freeze({
        version: 1 as const,
        signal: controller.signal,
        features: Object.freeze(["ui.controls/1", "ui.forms/1", "ui.forms/2", "ui.controls/2", "ui.feedback/1", "ui.layout/1", "ui.tabs/1", "ui.dialog/1", "ui.charts/1", "ui.assets/1", ...(descriptor.settings_provider && (application || slot === "settings") ? ["ui.settings/1"] : []), ...(application ? ["ui.application-section/1", ...(descriptor.settings_provider ? ["ui.application-settings/1"] : [])] : [])]),
        ui: Object.freeze({
          ...createExtensionUiWithDialogsV1(controller.signal),
          ...createExtensionUiTabsV1(controller.signal),
          ...createExtensionUiChartsV1(controller.signal),
        }),
        assets: Object.freeze({ stylesheet: (path: string) => loadExtensionStylesheet(id, descriptor.digest, path, controller.signal) }),
        canCall: (method: string) => !controller.signal.aborted && aliases.has(method),
        call: (method: string, payload: Payload = {}) => {
          if (controller.signal.aborted || !aliases.has(method)) return Promise.reject(new Error("Extension method is unavailable."));
          return this.api.post<{ result: unknown }>("extensions/call", { id, digest: descriptor.digest, method, payload }, true, controller.signal)
            .then((reply) => {
              if (!Object.hasOwn(reply, "result")) throw new Error("Invalid extension response.");
              return reply.result;
            });
        },
        setDirty: (value: boolean) => { if (typeof value !== "boolean") throw new Error("Invalid plugin draft state."); if (!controller.signal.aborted) { this.dirty = value; state?.("dirty", value); } },
        setBusy: (value: boolean) => { if (typeof value !== "boolean") throw new Error("Invalid plugin busy state."); if (!controller.signal.aborted) { this.busy = value; state?.("busy", value); } },
        ...(application ? {
          application: Object.freeze({ id: application.id, name: application.name,
            notifyChanged: () => {if (!controller.signal.aborted) application.onDocumentChange?.();} }),
        } : {}),
        ...(descriptor.settings_provider && (application || slot === "settings") ? {
          settings: this.scopedSettings(descriptor.settings_provider, application ? {kind:"application",id:application.id} : {kind:"host"},controller.signal),
        } : {}),
      });
      const callback = slot === "overview" ? page.mountOverview : slot === "settings" ? page.mountSettings : application ? page.mountApplication : page.mount;
      if (typeof callback !== "function") throw new Error("Extension did not register this UI location.");
      try {
        const cleanup = callback(container, sdk);
        if (typeof cleanup === "function") {
          if (controller.signal.aborted) cleanup();
          else this.cleanup = cleanup;
        }
      } catch (error) {
        this.dispose();
        throw error;
      }
    }

    async mountSlot(id: string, container: HTMLElement, slot: "overview" | "settings"): Promise<void> {
      return this.mount(id, container, undefined, undefined, slot);
    }

    private scopedSettings(provider: string, selectedScope: ExtensionSettingsScope, signal: AbortSignal): ApplicationSettingsApi {
      const scope = Object.freeze(selectedScope);
      const request = async <T>(route: string, payload: Payload): Promise<T> => {
        if (signal.aborted) throw new Error("Plugin settings section is closed.");
        const response = await this.api.post<T>(route, { ...payload, provider, scope }, true, signal);
        if (signal.aborted) throw new Error("Plugin settings section is closed.");
        return response;
      };
      return Object.freeze({
        version: 1 as const,
        provider,
        scope,
        canWrite: this.api.allows("configuration.write"),
        describe: async () => {
          let after: string | null = null, revision: number | null = null;
          const seen = new Set<string>();
          for (let pageIndex = 0; pageIndex < 32; pageIndex++) {
            if (signal.aborted) throw new Error("Plugin settings section is closed.");
            const page: {items:ExtensionSettingsDescriptor[];next:string|null;revision:number} = await this.api.post("session-extensions/list",{after,limit:32,revision},true,signal);
            if (signal.aborted) throw new Error("Plugin settings section is closed.");
            if (!Array.isArray(page.items) || page.items.length > 32 || !Number.isSafeInteger(page.revision) || page.revision < 0 ||
                (revision !== null && revision !== page.revision) || (page.next !== null && typeof page.next !== "string")) throw new Error("Invalid plugin settings discovery.");
            const descriptor = page.items.find(item => item.provider === provider);
            if (descriptor) return descriptor;
            if (page.next === null) throw new Error("Plugin settings provider is unavailable.");
            if (!page.next || seen.has(page.next)) throw new Error("Plugin settings discovery cursor repeated.");
            seen.add(page.next); after = page.next; revision = page.revision;
          }
          throw new Error("Plugin settings discovery exceeded its bound.");
        },
        read: () => request<ApplicationSettingsDocument>("session-extensions/settings_get", {}),
        update: (expectedRevision: number, values: Payload) => {
          if (!this.api.allows("configuration.write")) return Promise.reject(new Error("Plugin settings are read-only."));
          return request<{ revision: number }>("session-extensions/settings_update", { expected_revision: expectedRevision, values });
        },
      });
    }
  }
}
