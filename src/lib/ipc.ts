import { Channel, invoke } from "@tauri-apps/api/core";

/** True when running inside the Tauri webview (vs. a plain browser dev harness). */
export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface SpawnOpts {
  command?: string | null;
  args?: string[];
  cwd?: string | null;
  rows: number;
  cols: number;
}

export interface TabRecord {
  title: string;
  cwd: string;
  sort_order?: number;
  is_active?: boolean;
  shell_profile_id?: string | null;
}

export interface Theme {
  background: string;
  foreground: string;
  cursor: string;
  selection: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  bright_black: string;
  bright_red: string;
  bright_green: string;
  bright_yellow: string;
  bright_blue: string;
  bright_magenta: string;
  bright_cyan: string;
  bright_white: string;
}

export interface ShellProfile {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd?: string | null;
}

export interface AppConfig {
  font_family: string;
  font_size: number;
  theme: Theme;
  shell_profiles: ShellProfile[];
  default_shell_profile_id: string;
  restore_on_launch: boolean;
  /** In-memory scrollback lines per terminal; never persisted to disk. */
  scrollback_lines: number;
}

/** Map our snake_case Theme onto ghostty-web's camelCase Terminal theme. */
export function ghosttyTheme(t: Theme): Record<string, string> {
  return {
    background: t.background,
    foreground: t.foreground,
    cursor: t.cursor,
    selectionBackground: t.selection,
    black: t.black,
    red: t.red,
    green: t.green,
    yellow: t.yellow,
    blue: t.blue,
    magenta: t.magenta,
    cyan: t.cyan,
    white: t.white,
    brightBlack: t.bright_black,
    brightRed: t.bright_red,
    brightGreen: t.bright_green,
    brightYellow: t.bright_yellow,
    brightBlue: t.bright_blue,
    brightMagenta: t.bright_magenta,
    brightCyan: t.bright_cyan,
    brightWhite: t.bright_white,
  };
}

/** Resolve a profile id against config, falling back to the default profile. */
export function resolveProfile(
  config: AppConfig,
  id: string | null | undefined,
): ShellProfile | undefined {
  const profiles = config.shell_profiles;
  return (
    profiles.find((p) => p.id === id) ??
    profiles.find((p) => p.id === config.default_shell_profile_id) ??
    profiles[0]
  );
}

/**
 * Spawn a PTY in the Rust backend. Output bytes are streamed over an ipc Channel
 * and handed to `onBytes`. Returns the backend PTY id.
 */
export async function spawnPty(
  opts: SpawnOpts,
  onBytes: (data: Uint8Array) => void,
): Promise<number> {
  const channel = new Channel<number[]>();
  channel.onmessage = (msg) => onBytes(Uint8Array.from(msg));
  return invoke<number>("pty_spawn", { opts, onData: channel });
}

export const ptyWrite = (id: number, data: Uint8Array): Promise<void> =>
  invoke("pty_write", { id, data: Array.from(data) });

export const ptyResize = (id: number, rows: number, cols: number): Promise<void> =>
  invoke("pty_resize", { id, rows, cols });

export const ptyKill = (id: number): Promise<void> => invoke("pty_kill", { id });

export const ptyCwd = (id: number): Promise<string | null> => invoke("pty_cwd", { id });

export const tabsLoad = (): Promise<TabRecord[]> => invoke("tabs_load");

export const tabsSave = (tabs: TabRecord[]): Promise<void> => invoke("tabs_save", { tabs });

export const configGet = (): Promise<AppConfig> => invoke("config_get");

export const configSet = (config: AppConfig): Promise<void> => invoke("config_set", { config });

export const homeDir = (): Promise<string | null> => invoke("home_dir");
