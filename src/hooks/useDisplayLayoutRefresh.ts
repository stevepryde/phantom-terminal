import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";
import { isTauri } from "../lib/ipc";

/**
 * Moving a WKWebView between macOS displays can leave custom draggable chrome in
 * a stale paint state until the next resize. Nudge chrome + terminals after
 * display-affecting window events so crossing screens behaves like a resize.
 *
 * Returns a paint revision that callers spread into chrome components to force a
 * repaint; the hook also emits a `phantom:display-layout-change` event that
 * terminals listen for to re-fit.
 */
export function useDisplayLayoutRefresh(): number {
  const [paintRevision, setPaintRevision] = useState(0);

  useEffect(() => {
    if (!isTauri()) return;

    const win = getCurrentWindow();
    let timers: Array<ReturnType<typeof setTimeout>> = [];
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const clearRefreshTimers = () => {
      for (const timer of timers) clearTimeout(timer);
      timers = [];
    };

    const emitDisplayLayoutRefresh = () => {
      if (disposed) return;
      setPaintRevision((revision) => revision + 1);
      window.dispatchEvent(new Event("phantom:display-layout-change"));
    };

    const refreshDisplayLayout = () => {
      clearRefreshTimers();
      timers = [0, 50, 150, 300].map((delay) => setTimeout(emitDisplayLayoutRefresh, delay));
    };

    void Promise.all([
      win.onScaleChanged(refreshDisplayLayout),
      win.onMoved(refreshDisplayLayout),
      win.onResized(refreshDisplayLayout),
      win.onFocusChanged(({ payload: focused }) => {
        if (focused) refreshDisplayLayout();
      }),
    ]).then((listeners) => {
      if (disposed) {
        for (const unlisten of listeners) unlisten();
      } else {
        unlisteners.push(...listeners);
      }
    });

    return () => {
      disposed = true;
      clearRefreshTimers();
      for (const unlisten of unlisteners) unlisten();
    };
  }, []);

  return paintRevision;
}
