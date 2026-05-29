import { getCurrentWindow } from "@tauri-apps/api/window";
import { Maximize2, Minus, X } from "lucide-react";
import type { ReactNode } from "react";
import { isTauri } from "../lib/ipc";
import { IconButton } from "./IconButton";

interface Props {
  placement?: "leading" | "trailing";
}

/**
 * Min / maximize / close buttons for the custom (decorationless) title bar.
 * Rendered only inside Tauri — in a plain browser preview there is no window to
 * control, so we render nothing rather than throw.
 */
export function WindowControls({ placement = "trailing" }: Props) {
  if (!isTauri()) return null;
  const win = getCurrentWindow();
  const macOS = isMacOS();

  if (macOS) {
    if (placement !== "leading") return null;

    return (
      <div className="no-drag flex h-full shrink-0 items-center gap-2 px-3">
        <TrafficLightButton
          label="Close"
          className="bg-[#ff5f57] text-red-950/90"
          onClick={() => void win.close()}
        >
          <X size={8} strokeWidth={3} />
        </TrafficLightButton>
        <TrafficLightButton
          label="Minimize"
          className="bg-[#febc2e] text-yellow-950/90"
          onClick={() => void win.minimize()}
        >
          <Minus size={8} strokeWidth={3} />
        </TrafficLightButton>
        <TrafficLightButton
          label="Maximize"
          className="bg-[#28c840] text-green-950/90"
          onClick={() => void win.toggleMaximize()}
        >
          <Maximize2 size={7} strokeWidth={3} />
        </TrafficLightButton>
      </div>
    );
  }

  if (placement !== "trailing") return null;

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

function isMacOS() {
  if (typeof navigator === "undefined") return false;
  return navigator.platform.toLowerCase().includes("mac");
}

interface TrafficLightButtonProps {
  label: string;
  className: string;
  onClick: () => void;
  children: ReactNode;
}

function TrafficLightButton({ label, className, onClick, children }: TrafficLightButtonProps) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={`group flex h-3 w-3 items-center justify-center rounded-full border border-black/25 shadow-[inset_0_0_0_0.5px_rgba(255,255,255,0.35)] outline-none focus-visible:ring-2 focus-visible:ring-white/50 ${className}`}
    >
      <span className="opacity-0 transition-opacity group-hover:opacity-70 group-focus-visible:opacity-70">
        {children}
      </span>
    </button>
  );
}
