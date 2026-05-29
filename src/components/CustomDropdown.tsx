import { Check, ChevronDown } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

export interface DropdownOption {
  value: string;
  label: string;
}

interface Props {
  value: string;
  options: DropdownOption[];
  onChange: (value: string) => void;
  placeholder?: string;
  className?: string;
}

/**
 * A keyboard-accessible select replacement. We roll our own (instead of native
 * <select>) so the menu matches the app theme; closes on outside click / Escape.
 */
export function CustomDropdown({ value, options, onChange, placeholder, className = "" }: Props) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const listId = useId();
  const selected = options.find((o) => o.value === value);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={rootRef} className={`relative ${className}`}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-2 rounded border border-white/15 bg-white/5 px-3 py-1.5 text-left text-sm text-white hover:border-white/30"
      >
        <span className={selected ? "" : "text-white/40"}>
          {selected?.label ?? placeholder ?? "Select…"}
        </span>
        <ChevronDown size={14} className="shrink-0 text-white/50" />
      </button>
      {open && (
        <div
          id={listId}
          className="absolute z-50 mt-1 max-h-64 w-full overflow-auto rounded border border-white/15 bg-[#1a1a20] py-1 shadow-xl"
        >
          {options.map((opt) => {
            const active = opt.value === value;
            return (
              <button
                key={opt.value}
                type="button"
                aria-pressed={active}
                onClick={() => {
                  onChange(opt.value);
                  setOpen(false);
                }}
                className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-sm text-white/80 hover:bg-white/10"
              >
                <span>{opt.label}</span>
                {active && <Check size={14} className="text-white" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
