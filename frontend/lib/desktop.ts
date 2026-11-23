export type DesktopRunnerStatus = "unpaired" | "paused" | "connecting" | "idle" | "running" | "offline" | "error";

export interface DesktopRepository {
  id: string;
  name: string;
  path: string;
}

export interface DesktopBackend {
  name: string;
  installed: boolean;
  authenticated: boolean | null;
}

export interface DesktopSlack {
  enabled: boolean;
  running: boolean;
  workspaces: string[];
  delivered: number;
  last_error: string | null;
}

export interface DesktopStatus {
  available: true;
  runner_id: string | null;
  name: string;
  status: DesktopRunnerStatus;
  server: string;
  workspace_root: string;
  repositories: DesktopRepository[];
  backends: DesktopBackend[];
  last_error: string | null;
  start_at_login?: boolean;
  slack?: DesktopSlack;
  features?: string[];
  terminals_per_place?: number;
}

type DesktopCommand =
  | "folder_for_path"
  | "reveal_path"
  | "desktop_status"
  | "pair_device"
  | "pause_runner"
  | "resume_runner"
  | "disconnect_device"
  | "choose_repository"
  | "open_runner_logs"
  | "set_start_at_login"
  | "set_slack_capture"
  | "folders_list"
  | "folder_add"
  | "folder_remove"
  | "update_status"
  | "update_check"
  | "update_install"
  | "update_when_idle"
  | "update_stop_waiting"
  | "update_parts_apply"
  | "update_parts_auto"
  | "update_auto"
  | "upgrade_start"
  | "upgrade_status"
  | "upgrade_cancel"
  | "upgrade_retry";

export interface UpdateView {
  state: "idle" | "checking" | "downloading" | "ready" | "waiting" | "current" | "failed" | "installing";
  current: string;
  version: string | null;
  notes: string | null;
  progress: number | null;
  error: string | null;
  busy?: number;
  auto_install?: boolean;
  parts?: UpdateParts | null;
  auto_parts?: boolean;
  parts_enabled?: boolean;
}

export interface UpdateParts {
  state: "available" | "applying" | "applied" | "failed";
  version: string | null;
  notes: string | null;
  names: ("api" | "web")[];
  step: string | null;
  error: string | null;
  reload: boolean;
}

export const UPDATE_EVENT = "world:update";

export type UpgradeStep =
  | "idle"
  | "waiting"
  | "checking"
  | "packing"
  | "moving"
  | "downloading"
  | "verifying"
  | "installing"
  | "opening"
  | "done"
  | "moved"
  | "download_failed"
  | "check_failed"
  | "move_failed"
  | "timed_out"
  | "install_blocked"
  | "open_failed";

export type UpgradeAction = "page" | "again" | "download" | "close" | "open" | "finder";

export interface UpgradeView {
  step: UpgradeStep;
  org: string | null;
  version: string | null;
  done: number | null;
  total: number | null;
  error: string | null;
  agents: number | null;
  reports: number | null;
  keys: number | null;
  renamed: string[];
  needs_values: string[];
  notes: string[];
  download_page: string | null;
  can_retry: boolean;
  blocked: "running" | "other" | null;
  login_note: boolean;
}

export const UPGRADE_EVENT = "world:upgrade";

export interface SharedFolder {
  id: string;
  name: string;
  path: string;
}

export interface PlacedPath {
  folder: SharedFolder;
  file: string | null;
  is_dir: boolean;
  shared_now: boolean;
}

declare global {
  interface Window {
    __TAURI__?: {
      core?: {
        invoke?: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
        Channel?: new <T>() => { onmessage: (message: T) => void };
      };
    };
  }
}

function invoke<T>(command: DesktopCommand, args?: Record<string, unknown>): Promise<T> {
  const call = typeof window !== "undefined" ? window.__TAURI__?.core?.invoke : undefined;
  if (!call) return Promise.reject(new Error("desktop_unavailable"));
  return call<T>(command, args);
}

export function isDesktopApp(): boolean {
  return typeof window !== "undefined" && typeof window.__TAURI__?.core?.invoke === "function";
}

export const desktopApi = {
  status: () => invoke<DesktopStatus>("desktop_status"),
  pair: (name: string) => invoke<DesktopStatus>("pair_device", { name }),
  pause: () => invoke<DesktopStatus>("pause_runner"),
  resume: () => invoke<DesktopStatus>("resume_runner"),
  disconnect: () => invoke<DesktopStatus>("disconnect_device"),
  chooseRepository: () => invoke<DesktopStatus>("choose_repository"),
  openLogs: () => invoke<void>("open_runner_logs"),
  setStartAtLogin: (enabled: boolean) => invoke<DesktopStatus>("set_start_at_login", { enabled }),
  setSlackCapture: (enabled: boolean) => invoke<DesktopStatus>("set_slack_capture", { enabled }),
  folders: () => invoke<SharedFolder[]>("folders_list"),
  addFolder: () => invoke<SharedFolder | null>("folder_add"),
  removeFolder: (id: string) => invoke<SharedFolder[]>("folder_remove", { id }),
  folderForPath: (path: string) => invoke<PlacedPath>("folder_for_path", { path }),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  updateStatus: () => invoke<UpdateView>("update_status"),
  checkForUpdates: () => invoke<void>("update_check"),
  installUpdate: () => invoke<void>("update_install"),
  updateWhenIdle: () => invoke<void>("update_when_idle"),
  stopWaitingForUpdate: () => invoke<void>("update_stop_waiting"),
  setAutoUpdate: (enabled: boolean) => invoke<UpdateView>("update_auto", { enabled }),
  applyUpdateParts: () => invoke<void>("update_parts_apply"),
  setAutoUpdateParts: (enabled: boolean) => invoke<UpdateView>("update_parts_auto", { enabled }),
  upgradeStart: () => invoke<UpgradeView>("upgrade_start"),
  upgradeStatus: () => invoke<UpgradeView>("upgrade_status"),
  upgradeCancel: () => invoke<UpgradeView>("upgrade_cancel"),
  upgradeRetry: (action: UpgradeAction) => invoke<UpgradeView>("upgrade_retry", { action }),
};
