import type { LucideIcon } from "lucide-react";

interface Props {
  icon: LucideIcon;
  label: string;
  onClick?: () => void;
  title?: string;
  className?: string;
  size?: number;
}

/** A square, accessible icon-only button used in the tab bar and chrome. */
export function IconButton({
  icon: Icon,
  label,
  onClick,
  title,
  className = "",
  size = 16,
}: Props) {
  return (
    <button
      type="button"
      aria-label={label}
      title={title ?? label}
      onClick={onClick}
      className={`no-drag flex items-center justify-center rounded text-white/60 hover:bg-white/10 hover:text-white ${className}`}
    >
      <Icon size={size} strokeWidth={2} />
    </button>
  );
}
