import { useEffect } from "react";
import { ptyCwd } from "../lib/ipc";
import { setTabCwd, tabsStore } from "../store/tabs";

/**
 * The single owner of live cwd: poll each tab's PTY so cwd-named tabs track
 * `cd`. `setTabCwd` is a no-op when the path is unchanged, so this only mutates
 * the store on a real directory change. The session saver reads the resulting
 * `tab.cwd` rather than resolving cwd again, so this is the only place that
 * queries `ptyCwd` for tracking (ROADMAP MAINT-4).
 */
export function useLiveCwdPolling(intervalMs = 1500) {
  useEffect(() => {
    const id = setInterval(async () => {
      for (const tab of tabsStore.state.tabs) {
        if (tab.ptyId == null) continue;
        const live = await ptyCwd(tab.ptyId);
        if (live) setTabCwd(tab.id, live);
      }
    }, intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
}
