import { getCurrentWindow } from "@tauri-apps/api/window";
import { Maximize2, Minus, X } from "lucide-react";
import { isTauri } from "../lib/ipc";
import { IconButton } from "./IconButton";

/**
 * Min / maximize / close buttons for the custom (decorationless) title bar.
 * Rendered only inside Tauri — in a plain browser preview there is no window to
 * control, so we render nothing rather than throw.
 */
export function WindowControls() {
  if (!isTauri()) return null;
  const win = getCurrentWindow();
  return (
    <div className="no-drag flex items-stretch">
      <IconButton
        icon={Minus}
        label="Minimize"
        className="h-7 w-9"
        onClick={() => void win.minimize()}
      />
      <IconButton
        icon={Maximize2}
        label="Maximize"
        size={13}
        className="h-7 w-9"
        onClick={() => void win.toggleMaximize()}
      />
      <IconButton
        icon={X}
        label="Close"
        className="h-7 w-9 hover:bg-red-500/80 hover:text-white"
        onClick={() => void win.close()}
      />
    </div>
  );
}
