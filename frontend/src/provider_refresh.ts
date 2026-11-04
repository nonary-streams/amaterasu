namespace Amaterasu {
  export interface ProviderRefreshApi {
    post<T = unknown>(path: string): Promise<T>;
  }
  export async function refreshLutrisProvider(
    api: ProviderRefreshApi,
    accepted?: () => void,
  ): Promise<LutrisStatus> {
    const status = await api.post<LutrisStatus>("lutris/refresh");
    if (!status || status.ok !== true) throw new Error("Lutris refresh was not accepted.");
    accepted?.();
    return status;
  }
  export async function refreshPlayniteProvider(
    api: ProviderRefreshApi,
    accepted?: () => void,
  ): Promise<PlayniteStatus> {
    const response = await api.post<unknown>("playnite/refresh");
    if (!response || typeof response !== "object" || Array.isArray(response) ||
        (response as Record<string, unknown>).ok !== true)
      throw new Error("Playnite refresh was not accepted.");
    accepted?.();
    return api.post<PlayniteStatus>("playnite/status");
  }
}
