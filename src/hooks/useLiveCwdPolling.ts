import { useEffect } from "react";
import { ptyCwd } from "../lib/ipc";
import { setTabCwd, tabsStore } from "../store/tabs";

const CWD_ACTIVITY_EVENT = "phantom:terminal-cwd-activity";
const IMMEDIATE_POLL_DELAY_MS = 120;
const ACTIVE_POLL_INTERVAL_MS = 2500;
const ACTIVE_WINDOW_MS = 30_000;

interface CwdActivityDetail {
  tabId: string;
}

/**
 * Request live cwd refreshes around terminal activity. Shells do not expose a
 * portable "command finished" signal without shell integration, so the app uses
 * a bounded activity window: poll shortly after input/output, keep polling while
 * the command may be running, then go fully idle again.
 */
export function requestLiveCwdRefresh(tabId: string) {
  window.dispatchEvent(
    new CustomEvent<CwdActivityDetail>(CWD_ACTIVITY_EVENT, { detail: { tabId } }),
  );
}

/**
 * The single owner of live cwd tracking. `setTabCwd` is a no-op when the path is
 * unchanged, so this only mutates the store on a real directory change. The
 * session saver reads the resulting `tab.cwd` rather than resolving cwd again,
 * so this is the only place that queries `ptyCwd` for tab-title tracking.
 */
export function useLiveCwdPolling(
  activePollIntervalMs = ACTIVE_POLL_INTERVAL_MS,
  activeWindowMs = ACTIVE_WINDOW_MS,
) {
  useEffect(() => {
    const entries = new Map<
      string,
      {
        polling: boolean;
        interval: ReturnType<typeof setInterval> | null;
        immediate: ReturnType<typeof setTimeout> | null;
        expires: ReturnType<typeof setTimeout> | null;
      }
    >();

    const clearEntry = (tabId: string) => {
      const entry = entries.get(tabId);
      if (!entry) return;
      if (entry.interval) clearInterval(entry.interval);
      if (entry.immediate) clearTimeout(entry.immediate);
      if (entry.expires) clearTimeout(entry.expires);
      entries.delete(tabId);
    };

    const pollTab = async (tabId: string) => {
      const entry = entries.get(tabId);
      if (!entry || entry.polling) return;
      const tab = tabsStore.state.tabs.find((t) => t.id === tabId);
      if (!tab || tab.ptyId == null) {
        clearEntry(tabId);
        return;
      }

      entry.polling = true;
      try {
        const live = await ptyCwd(tab.ptyId);
        if (live) setTabCwd(tab.id, live);
      } catch {
        // Exit/write handling owns dead-terminal UI; cwd polling is best-effort.
      } finally {
        entry.polling = false;
      }
    };

    const onActivity = (event: Event) => {
      const tabId = (event as CustomEvent<CwdActivityDetail>).detail?.tabId;
      if (!tabId) return;

      let entry = entries.get(tabId);
      if (!entry) {
        entry = {
          polling: false,
          interval: null,
          immediate: null,
          expires: null,
        };
        entries.set(tabId, entry);
      }

      if (entry.immediate) clearTimeout(entry.immediate);
      entry.immediate = setTimeout(() => {
        entry.immediate = null;
        void pollTab(tabId);
      }, IMMEDIATE_POLL_DELAY_MS);

      entry.interval ??= setInterval(() => {
        void pollTab(tabId);
      }, activePollIntervalMs);

      if (entry.expires) clearTimeout(entry.expires);
      entry.expires = setTimeout(() => clearEntry(tabId), activeWindowMs);
    };

    window.addEventListener(CWD_ACTIVITY_EVENT, onActivity);
    return () => {
      window.removeEventListener(CWD_ACTIVITY_EVENT, onActivity);
      for (const tabId of entries.keys()) clearEntry(tabId);
    };
  }, [activePollIntervalMs, activeWindowMs]);
}
