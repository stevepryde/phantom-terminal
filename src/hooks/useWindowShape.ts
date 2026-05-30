import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";
import { isTauri } from "../lib/ipc";

/**
 * Track whether the window is maximized or fullscreen. Rounded transparent
 * windows should square off in those states so the content still reaches the
 * display edges.
 */
export function useWindowShape(): boolean {
  const [windowMaximized, setWindowMaximized] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;

    const win = getCurrentWindow();
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const updateWindowShape = () => {
      void Promise.all([win.isMaximized(), win.isFullscreen()])
        .then(([maximized, fullscreen]) => {
          if (!disposed) setWindowMaximized(maximized || fullscreen);
        })
        .catch((err) => {
          console.error("phantom: failed to read window shape state", err);
        });
    };

    updateWindowShape();

    void Promise.all([
      win.onResized(updateWindowShape),
      win.onMoved(updateWindowShape),
      win.onFocusChanged(updateWindowShape),
    ]).then((listeners) => {
      if (disposed) {
        for (const unlisten of listeners) unlisten();
      } else {
        unlisteners.push(...listeners);
      }
    });

    return () => {
      disposed = true;
      for (const unlisten of unlisteners) unlisten();
    };
  }, []);

  return windowMaximized;
}
