import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Settings } from "@/bindings/Settings";
import { api } from "@/lib/api";
import { useApp } from "@/stores/app";

export const qk = {
  appInfo: ["appInfo"] as const,
  settings: ["settings"] as const,
  index: ["index"] as const,
  indexCache: ["indexCache"] as const,
  drives: ["drives"] as const,
  driveState: (path: string | null) => ["driveState", path] as const,
  check: (path: string | null, profile: string | null) => ["check", path, profile] as const,
  cache: ["cache"] as const,
};

export function useAppInfo() {
  return useQuery({ queryKey: qk.appInfo, queryFn: () => api.appInfo(), staleTime: Infinity });
}

export function useSettings() {
  return useQuery({ queryKey: qk.settings, queryFn: () => api.getSettings(), staleTime: Infinity });
}

export function useSaveSettings() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (settings: Settings) => api.setSettings(settings),
    onSuccess: (saved) => qc.setQueryData(qk.settings, saved),
  });
}

/**
 * The package index: the saved copy right away, then the network copy.
 * Refreshed every 30 minutes while the app is open.
 */
export function useIndex() {
  const cached = useQuery({ queryKey: qk.indexCache, queryFn: () => api.cachedIndex(), staleTime: Infinity });
  const live = useQuery({
    queryKey: qk.index,
    queryFn: () => api.fetchIndex(),
    // Wait for the cache read so the Rust side has an index in memory first.
    enabled: cached.isFetched,
    staleTime: 5 * 60_000,
    refetchInterval: 30 * 60_000,
    retry: 1,
  });
  return {
    result: live.data ?? cached.data ?? null,
    isLoading: !live.data && !cached.data && (cached.isLoading || live.isLoading),
    isFetching: live.isFetching,
    error: live.error,
    refetch: live.refetch,
  };
}

/** Removable drives, polled every 2 s while the window is visible. */
export function useDrives() {
  return useQuery({
    queryKey: qk.drives,
    queryFn: () => api.listDrives(),
    refetchInterval: 2000,
    refetchIntervalInBackground: false,
  });
}

export function useDriveState() {
  const path = useApp((s) => s.drivePath);
  const { result } = useIndex();
  return useQuery({
    queryKey: [...qk.driveState(path), result?.index.generatedAt],
    queryFn: () => api.readDrive(path!),
    enabled: !!path && !!result,
    retry: false,
  });
}

export function useCheck() {
  const path = useApp((s) => s.drivePath);
  const profile = useApp((s) => s.profileId);
  const { result } = useIndex();
  return useQuery({
    queryKey: [...qk.check(path, profile), result?.index.generatedAt],
    queryFn: () => api.checkDrive(path!, profile!),
    enabled: !!path && !!profile && !!result,
    retry: false,
  });
}

export function useCacheInfo() {
  return useQuery({ queryKey: qk.cache, queryFn: () => api.cacheInfo() });
}

/** Re-reads everything that depends on what's on the drive. */
export function useRefreshDrive() {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: ["driveState"] });
    void qc.invalidateQueries({ queryKey: ["check"] });
    void qc.invalidateQueries({ queryKey: qk.drives });
    void qc.invalidateQueries({ queryKey: qk.cache });
  };
}
