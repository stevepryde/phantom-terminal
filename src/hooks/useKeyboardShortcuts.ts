import { useEffect, useRef } from "react";
import type { AppConfig } from "../lib/ipc";
import { type KeybindingAction, resolveShortcut } from "../lib/keybindings";

export interface ShortcutHandlers {
  toggleSettings: () => void;
  togglePalette: () => void;
  newTab: () => void;
  closeActiveTab: () => void;
  renameActiveTab: () => void;
  activateRelative: (delta: number) => void;
  activateIndex: (index: number) => void;
}

/**
 * Bind global keyboard shortcuts once for the app's lifetime. Config and
 * handlers are read through a ref so changing a keybinding (or a handler
 * closure) takes effect without re-binding the listener. The pure
 * `resolveShortcut` does the matching; this hook only dispatches and consumes.
 */
export function useKeyboardShortcuts(config: AppConfig | null, handlers: ShortcutHandlers) {
  const latest = useRef({ config, handlers });
  latest.current = { config, handlers };

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const { config, handlers } = latest.current;
      const command = resolveShortcut(e, config?.keybindings ?? []);
      if (!command) return;
      consumeShortcut(e);
      switch (command.kind) {
        case "action":
          dispatchAction(command.action, handlers);
          break;
        case "activateRelative":
          handlers.activateRelative(command.delta);
          break;
        case "activateIndex":
          handlers.activateIndex(command.index);
          break;
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);
}

function dispatchAction(action: KeybindingAction, handlers: ShortcutHandlers) {
  switch (action) {
    case "settings.toggle":
      handlers.toggleSettings();
      break;
    case "palette.toggle":
      handlers.togglePalette();
      break;
    case "tab.new":
      handlers.newTab();
      break;
    case "tab.close":
      handlers.closeActiveTab();
      break;
    case "tab.rename":
      handlers.renameActiveTab();
      break;
  }
}

function consumeShortcut(event: KeyboardEvent) {
  event.preventDefault();
  event.stopPropagation();
  event.stopImmediatePropagation();
}
