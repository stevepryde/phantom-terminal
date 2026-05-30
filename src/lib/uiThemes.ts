import type { UiThemeId } from "./ipc";

export const UI_THEME_OPTIONS: Array<{ value: UiThemeId; label: string }> = [
  { value: "phantom", label: "Phantom" },
  { value: "aurora", label: "Aurora" },
  { value: "ember", label: "Ember" },
  { value: "cobalt", label: "Cobalt" },
  { value: "verdant", label: "Verdant" },
  { value: "violet", label: "Violet" },
  { value: "amethyst", label: "Amethyst" },
  { value: "ultraviolet", label: "Ultraviolet" },
  { value: "sapphire", label: "Sapphire" },
  { value: "glacier", label: "Glacier" },
  { value: "lagoon", label: "Lagoon" },
  { value: "emerald", label: "Emerald" },
  { value: "jade", label: "Jade" },
  { value: "silver", label: "Silver" },
];

export const UI_THEME_IDS = UI_THEME_OPTIONS.map((option) => option.value);
