namespace Amaterasu {
  export interface ProviderRefreshApi {
    post<T = unknown>(path: string): Promise<T>;
  }
  export function refreshLutrisProvider(api: ProviderRefreshApi): Promise<LutrisStatus> {
    return api.post<LutrisStatus>("lutris/refresh");
  }
  export async function refreshPlayniteProvider(api: ProviderRefreshApi): Promise<PlayniteStatus> {
    await api.post("playnite/refresh");
    return api.post<PlayniteStatus>("playnite/status");
  }
}
