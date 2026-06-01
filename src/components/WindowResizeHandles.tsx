import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "../lib/ipc";

type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

interface Props {
  disabled: boolean;
}

const handles: Array<{ direction: ResizeDirection; className: string; label: string }> = [
  { direction: "North", className: "window-resize-handle--north", label: "Resize north" },
  { direction: "East", className: "window-resize-handle--east", label: "Resize east" },
  { direction: "South", className: "window-resize-handle--south", label: "Resize south" },
  { direction: "West", className: "window-resize-handle--west", label: "Resize west" },
  {
    direction: "NorthEast",
    className: "window-resize-handle--north-east",
    label: "Resize north east",
  },
  {
    direction: "NorthWest",
    className: "window-resize-handle--north-west",
    label: "Resize north west",
  },
  {
    direction: "SouthEast",
    className: "window-resize-handle--south-east",
    label: "Resize south east",
  },
  {
    direction: "SouthWest",
    className: "window-resize-handle--south-west",
    label: "Resize south west",
  },
];

export function WindowResizeHandles({ disabled }: Props) {
  if (!isTauri() || disabled) return null;

  const win = getCurrentWindow();

  return (
    <div aria-hidden className="window-resize-handles no-drag">
      {handles.map((handle) => (
        <button
          key={handle.direction}
          type="button"
          aria-label={handle.label}
          tabIndex={-1}
          className={`window-resize-handle ${handle.className}`}
          onPointerDown={(e) => {
            if (e.button !== 0) return;
            e.preventDefault();
            e.stopPropagation();
            void win.startResizeDragging(handle.direction);
          }}
        />
      ))}
    </div>
  );
}
