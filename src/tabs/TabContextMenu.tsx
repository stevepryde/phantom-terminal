import { Pencil, Plus, X } from "lucide-react";
import { useEffect, useRef } from "react";

interface Props {
  x: number;
  y: number;
  onNewTab: () => void;
  onRename: () => void;
  onCloseTab: () => void;
  onDismiss: () => void;
}

/**
 * Right-click menu for a tab. Closes on outside click, Escape, or window blur.
 * Positioned at the cursor, clamped to stay within the viewport.
 */
export function TabContextMenu({ x, y, onNewTab, onRename, onCloseTab, onDismiss }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onDismiss();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onDismiss();
    }
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onDismiss);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onDismiss);
    };
  }, [onDismiss]);

  const left = Math.min(x, window.innerWidth - 196);
  const top = Math.min(y, window.innerHeight - 124);

  return (
    <div
      ref={ref}
      className="fixed z-50 w-48 overflow-hidden rounded-md border border-white/15 bg-[#1a1a20] py-1 shadow-xl"
      style={{ left, top }}
    >
      <button
        type="button"
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-white/80 hover:bg-white/10"
        onClick={onNewTab}
      >
        <Plus size={14} /> New tab after
      </button>
      <button
        type="button"
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-white/80 hover:bg-white/10"
        onClick={onRename}
      >
        <Pencil size={14} /> Rename
        <span className="ml-auto text-white/30 text-xs">F2</span>
      </button>
      <button
        type="button"
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-white/80 hover:bg-red-500/20 hover:text-white"
        onClick={onCloseTab}
      >
        <X size={14} /> Close
        <span className="ml-auto text-white/30 text-xs">⌘W</span>
      </button>
    </div>
  );
}
