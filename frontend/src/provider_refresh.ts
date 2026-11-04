namespace Amaterasu {
  export interface ProviderRefreshApi {
    post<T = unknown>(path: string): Promise<T>;
  }
  export async function refreshLutrisProvider(
    api: ProviderRefreshApi,
    accepted?: () => void,
  ): Promise<LutrisStatus> {
    const status = await api.post<LutrisStatus>("lutris/refresh");
    accepted?.();
    return status;
  }
  export async function refreshPlayniteProvider(
    api: ProviderRefreshApi,
    accepted?: () => void,
  ): Promise<PlayniteStatus> {
    await api.post("playnite/refresh");
    accepted?.();
    return api.post<PlayniteStatus>("playnite/status");
  }
}
