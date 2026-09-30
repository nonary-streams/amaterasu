namespace Amaterasu {
  function updaterTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function updaterTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  const maximumUpdateCheckIntervalSeconds = 0x7fff_ffff;
  const updaterRenderGenerations = new WeakMap<HTMLElement, number>();
  type UpdaterNotificationDelivery = "pending" | "notified" | "disabled" | "unavailable";

  export interface UpdaterSettings {
    notify_pre_releases: boolean;
    update_check_interval: number;
  }

  export interface UpdaterSettingsReply {
    ok: true;
    settings: UpdaterSettings;
    revision: number;
  }

  export interface UpdaterReleaseSummary {
    version: string;
    url: string;
  }

  export interface UpdaterStatusReply {
    ok: true;
    settings_ready: boolean;
    revision: number;
    check_in_progress: boolean;
    last_check_unix_ms: number | null;
    last_error: string | null;
    latest_stable: UpdaterReleaseSummary | null;
    latest_prerelease: UpdaterReleaseSummary | null;
    available_update: {
      version: string;
      release_url: string;
      prerelease: boolean;
    } | null;
    notification: {
      version: string;
      prerelease: boolean;
      release_url: string;
      delivery: UpdaterNotificationDelivery;
    } | null;
  }

  interface UpdaterCheckReply {
    ok: true;
    started: boolean;
    in_progress: boolean;
  }

  function updaterSettings(
    notifyPreReleases: unknown,
    updateCheckInterval: unknown,
  ): UpdaterSettings {
    if (
      typeof notifyPreReleases !== "boolean" ||
      typeof updateCheckInterval !== "number" ||
      !Number.isSafeInteger(updateCheckInterval) ||
      updateCheckInterval < 0 ||
      updateCheckInterval > maximumUpdateCheckIntervalSeconds
    )
      throw new Error("Update interval must be a whole number of seconds from 0 to 2147483647.");
    return {
      notify_pre_releases: notifyPreReleases,
      update_check_interval: updateCheckInterval,
    };
  }

  export function parseUpdaterSettingsReply(value: unknown): UpdaterSettingsReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Updater settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        !reply.settings || typeof reply.settings !== "object" || Array.isArray(reply.settings))
      throw new Error("Updater settings reply is invalid.");
    const settings = reply.settings as Record<string, unknown>;
    if (Object.keys(settings).length !== 2 ||
        !Object.hasOwn(settings, "notify_pre_releases") ||
        !Object.hasOwn(settings, "update_check_interval"))
      throw new Error("Updater settings reply is invalid.");
    return {
      ok: true,
      revision: reply.revision as number,
      settings: updaterSettings(settings.notify_pre_releases, settings.update_check_interval),
    };
  }

  function parseUpdaterReleaseSummary(value: unknown): UpdaterReleaseSummary | null {
    if (value === null) return null;
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Updater release status is invalid.");
    const release = value as Record<string, unknown>;
    if (typeof release.version !== "string" || typeof release.url !== "string" ||
        !release.url.startsWith("https://github.com/Nonary/amaterasu/releases/tag/") ||
        /[\u0000-\u0020\u007f-\u009f]/u.test(release.url))
      throw new Error("Updater release status is invalid.");
    return { version: release.version, url: release.url };
  }

  export function parseUpdaterStatusReply(value: unknown): UpdaterStatusReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Updater status reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.settings_ready !== "boolean" ||
        !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        typeof reply.check_in_progress !== "boolean" ||
        !(reply.last_check_unix_ms === null ||
          (typeof reply.last_check_unix_ms === "number" && Number.isSafeInteger(reply.last_check_unix_ms))) ||
        !(reply.last_error === null || typeof reply.last_error === "string"))
      throw new Error("Updater status reply is invalid.");

    let availableUpdate: UpdaterStatusReply["available_update"] = null;
    if (reply.available_update !== null) {
      if (!reply.available_update || typeof reply.available_update !== "object" ||
          Array.isArray(reply.available_update))
        throw new Error("Updater release status is invalid.");
      const update = reply.available_update as Record<string, unknown>;
      if (typeof update.version !== "string" || typeof update.prerelease !== "boolean" ||
          typeof update.release_url !== "string" ||
          !update.release_url.startsWith("https://github.com/Nonary/amaterasu/releases/tag/") ||
          /[\u0000-\u0020\u007f-\u009f]/u.test(update.release_url))
        throw new Error("Updater release status is invalid.");
      availableUpdate = {
        version: update.version,
        release_url: update.release_url,
        prerelease: update.prerelease,
      };
    }

    let notification: UpdaterStatusReply["notification"] = null;
    if (reply.notification !== null) {
      if (!reply.notification || typeof reply.notification !== "object" || Array.isArray(reply.notification))
        throw new Error("Updater notification status is invalid.");
      const state = reply.notification as Record<string, unknown>;
      if (typeof state.version !== "string" || typeof state.prerelease !== "boolean" ||
          typeof state.release_url !== "string" ||
          !state.release_url.startsWith("https://github.com/Nonary/amaterasu/releases/tag/") ||
          /[\u0000-\u0020\u007f-\u009f]/u.test(state.release_url) ||
          !["pending", "notified", "disabled", "unavailable"].includes(String(state.delivery)))
        throw new Error("Updater notification status is invalid.");
      notification = {
        version: state.version,
        prerelease: state.prerelease,
        release_url: state.release_url,
        delivery: state.delivery as UpdaterNotificationDelivery,
      };
    }

    return {
      ok: true,
      settings_ready: reply.settings_ready,
      revision: reply.revision as number,
      check_in_progress: reply.check_in_progress,
      last_check_unix_ms: reply.last_check_unix_ms as number | null,
      last_error: reply.last_error as string | null,
      latest_stable: parseUpdaterReleaseSummary(reply.latest_stable),
      latest_prerelease: parseUpdaterReleaseSummary(reply.latest_prerelease),
      available_update: availableUpdate,
      notification,
    };
  }

  async function readUpdaterSettings(api: Pick<Api, "post">): Promise<UpdaterSettingsReply> {
    return parseUpdaterSettingsReply(await api.post<unknown>("updater/settings"));
  }

  async function readUpdaterStatus(api: Pick<Api, "post">): Promise<UpdaterStatusReply> {
    return parseUpdaterStatusReply(await api.post<unknown>("updater/status"));
  }

  export async function saveUpdaterSettings(
    api: Pick<Api, "post">,
    current: UpdaterSettingsReply,
    settings: UpdaterSettings,
  ): Promise<UpdaterSettingsReply> {
    const desired = updaterSettings(settings.notify_pre_releases, settings.update_check_interval);
    const updated = parseUpdaterSettingsReply(await api.post<unknown>("updater/settings_update", {
      settings: desired,
      expected_revision: current.revision,
    }));
    if (updated.revision !== current.revision + 1 ||
        updated.settings.notify_pre_releases !== desired.notify_pre_releases ||
        updated.settings.update_check_interval !== desired.update_check_interval)
      throw new Error("Updater settings update could not be confirmed. Refresh and check the saved values.");
    const saved = await readUpdaterSettings(api);
    if (saved.revision !== updated.revision ||
        saved.settings.notify_pre_releases !== desired.notify_pre_releases ||
        saved.settings.update_check_interval !== desired.update_check_interval)
      throw new Error("Updater settings could not be confirmed after saving. Refresh and check the saved values.");
    return saved;
  }

  function renderUpdaterStatus(area: HTMLElement, status: UpdaterStatusReply): void {
    area.replaceChildren();
    if (!status.settings_ready) {
      area.append(el("p", "muted", "Loading updater preferences from durable settings…"));
      return;
    }
    if (status.check_in_progress) {
      area.append(el("p", "muted", "Checking GitHub releases…"));
    } else if (status.last_error) {
      area.append(el("p", "error", "The most recent release check could not be completed."));
    } else if (status.available_update) {
      const link = el("a", "", updaterTextLiteral(`Version ${status.available_update.version} is available${
        status.available_update.prerelease ? " (pre-release)" : ""
      }`));
      link.href = status.available_update.release_url;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      area.append(el("p", "", link));
    } else if (status.last_check_unix_ms !== null) {
      area.append(el("p", "muted", "No newer release was found."));
    } else {
      area.append(el("p", "muted", "No release check has completed yet."));
    }
    if (status.last_check_unix_ms !== null) {
      area.append(el("p", "muted", updaterTextLiteral(`Last checked ${new Date(status.last_check_unix_ms).toLocaleString()}.`)));
    }
    if (status.notification) {
      area.append(el("p", "muted", updaterTextLiteral(`Desktop notification: ${status.notification.delivery}.`)));
    }
  }

  export function renderUpdaterSettings(context: PageContext, container: HTMLElement): void {
    const generation = (updaterRenderGenerations.get(container) ?? 0) + 1;
    updaterRenderGenerations.set(container, generation);
    const isCurrent = (): boolean =>
      container.isConnected && updaterRenderGenerations.get(container) === generation;

    const load = async (): Promise<void> => {
      container.replaceChildren(el("p", "muted", "Loading update preferences…"));
      try {
        const [settings, updaterStatus] = await Promise.all([
          readUpdaterSettings(context.api),
          readUpdaterStatus(context.api),
        ]);
        if (!isCurrent()) return;

        const form = el("form", "settings-form");
        const notifyPreReleases = input("notify_pre_releases", "", "checkbox");
        notifyPreReleases.checked = settings.settings.notify_pre_releases;
        const updateInterval = input(
          "update_check_interval",
          String(settings.settings.update_check_interval),
          "number",
        );
        updateInterval.min = "0";
        updateInterval.max = String(maximumUpdateCheckIntervalSeconds);
        updateInterval.step = "60";
        updateInterval.required = true;
        const status = el("div");
        const releaseStatus = el("div");
        renderUpdaterStatus(releaseStatus, updaterStatus);
        const save = el("button", "button primary", "Save update preferences");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        const refresh = button("Refresh release status", () => void load());
        refresh.disabled = !context.api.allows("configuration.read");
        const check = button("Check for updates now", () => {
          void runAction(check, status, async () => {
            const reply = await context.api.post<unknown>("updater/check");
            if (!reply || typeof reply !== "object" || Array.isArray(reply) ||
                (reply as Record<string, unknown>).ok !== true ||
                typeof (reply as Record<string, unknown>).started !== "boolean" ||
                typeof (reply as Record<string, unknown>).in_progress !== "boolean")
              throw new Error("Updater check response is invalid.");
            const started = (reply as UpdaterCheckReply).started;
            status.replaceChildren(el("p", "muted", started
              ? "Release check started. Refresh status to see its result."
              : "A release check is already in progress."));
            const current = await readUpdaterStatus(context.api);
            if (isCurrent()) renderUpdaterStatus(releaseStatus, current);
          });
        });
        check.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Notify about pre-releases",
            notifyPreReleases,
            "Stable updates are always considered. Pre-releases are included when enabled or when this build is already on a pre-release channel.",
          ),
          field(
            "Update check interval (seconds)",
            updateInterval,
            "The default is 86400 seconds (24 hours). Set 0 to disable periodic checks; a manual check is still available.",
          ),
          el("p", "muted", "The interval is stored in seconds. The control advances in one-minute steps."),
          el("div", "actions", save, badge(updaterTextLiteral(`Revision ${settings.revision}`))),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          if (!form.reportValidity()) return;
          void runAction(save, status, async () => {
            try {
              await saveUpdaterSettings(context.api, settings, updaterSettings(
                notifyPreReleases.checked,
                updateInterval.valueAsNumber,
              ));
              context.notify(updaterTextMessage("Update preferences were saved and read back."));
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                context.notify(updaterTextMessage("Update preferences changed elsewhere. Review the latest values and save again."));
              }
              await load();
              throw error;
            }
            await load();
          });
        });
        container.replaceChildren(
          panel("Software updates", form, check, releaseStatus, status),
        );
      } catch (error) {
        if (isCurrent())
          container.replaceChildren(errorPanel(error, load));
      }
    };

    void load();
  }
}
