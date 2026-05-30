import type { TerminalBackgroundId } from "./ipc";

export const TERMINAL_BACKGROUND_OPTIONS: Array<{
  value: TerminalBackgroundId;
  label: string;
}> = [
  { value: "phantom", label: "Phantom" },
  { value: "dragon", label: "Dragon" },
  { value: "none", label: "None" },
];

export const TERMINAL_BACKGROUND_IDS = TERMINAL_BACKGROUND_OPTIONS.map((option) => option.value);
