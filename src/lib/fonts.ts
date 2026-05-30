export interface FontOption {
  value: string;
  label: string;
}

export const TERMINAL_FONT_OPTIONS: FontOption[] = [
  { value: "monospace", label: "System Monospace" },
  {
    value: 'ui-monospace, "SF Mono", SFMono-Regular, Menlo, Monaco, Consolas, monospace',
    label: "System UI Mono",
  },
  {
    value: '"JetBrains Mono", "JetBrainsMono Nerd Font", Menlo, Monaco, monospace',
    label: "JetBrains Mono",
  },
  { value: '"SF Mono", SFMono-Regular, Menlo, Monaco, monospace', label: "SF Mono" },
  { value: "Menlo, Monaco, Consolas, monospace", label: "Menlo" },
  { value: "Monaco, Menlo, Consolas, monospace", label: "Monaco" },
  { value: '"Berkeley Mono", Menlo, Monaco, monospace', label: "Berkeley Mono" },
  { value: '"Fira Code", Menlo, Monaco, monospace', label: "Fira Code" },
  { value: '"Cascadia Code", Menlo, Monaco, monospace', label: "Cascadia Code" },
  { value: "Hack, Menlo, Monaco, monospace", label: "Hack" },
  { value: "Iosevka, Menlo, Monaco, monospace", label: "Iosevka" },
  { value: '"Source Code Pro", Menlo, Monaco, monospace', label: "Source Code Pro" },
];

export function terminalFontFamilyForRendering(fontFamily: string): string {
  return terminalFontDropdownValue(fontFamily);
}

export function terminalFontDropdownValue(fontFamily: string): string {
  const normalized = normalizeFontFamily(fontFamily);
  return (
    TERMINAL_FONT_OPTIONS.find((option) => {
      const normalizedValue = normalizeFontFamily(option.value);
      return (
        normalizedValue === normalized ||
        normalizeFontFamily(option.label) === normalized ||
        normalizedValue.startsWith(`${normalized},`)
      );
    })?.value ??
    (fontFamily.trim() || "monospace")
  );
}

export function terminalFontOptions(fontFamily: string): FontOption[] {
  const selected = terminalFontDropdownValue(fontFamily);
  if (TERMINAL_FONT_OPTIONS.some((option) => option.value === selected)) {
    return TERMINAL_FONT_OPTIONS;
  }
  return [...TERMINAL_FONT_OPTIONS, { value: selected, label: selected }];
}

function normalizeFontFamily(fontFamily: string): string {
  return fontFamily.toLowerCase().replace(/["']/g, "").replace(/\s+/g, " ").trim();
}
