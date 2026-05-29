import { History, Palette, Plus, SlidersHorizontal, Terminal, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { CustomDropdown } from "../components/CustomDropdown";
import type { AppConfig, ShellProfile, Theme } from "../lib/ipc";

interface Props {
  config: AppConfig;
  onChange: (patch: Partial<AppConfig>) => void;
}

// The 18 plain color swatches (background/foreground live in their own row, and
// selection is alpha-capable so it gets a text-only field).
const COLOR_FIELDS: Array<{ key: keyof Theme; label: string }> = [
  { key: "cursor", label: "Cursor" },
  { key: "black", label: "Black" },
  { key: "red", label: "Red" },
  { key: "green", label: "Green" },
  { key: "yellow", label: "Yellow" },
  { key: "blue", label: "Blue" },
  { key: "magenta", label: "Magenta" },
  { key: "cyan", label: "Cyan" },
  { key: "white", label: "White" },
  { key: "bright_black", label: "Br. Black" },
  { key: "bright_red", label: "Br. Red" },
  { key: "bright_green", label: "Br. Green" },
  { key: "bright_yellow", label: "Br. Yellow" },
  { key: "bright_blue", label: "Br. Blue" },
  { key: "bright_magenta", label: "Br. Magenta" },
  { key: "bright_cyan", label: "Br. Cyan" },
  { key: "bright_white", label: "Br. White" },
];

// The settings sections, each rendered as a sidebar tab.
const SECTIONS = [
  { id: "appearance", label: "Appearance", icon: SlidersHorizontal },
  { id: "theme", label: "Theme", icon: Palette },
  { id: "profiles", label: "Shell Profiles", icon: Terminal },
  { id: "session", label: "Session", icon: History },
] as const;
type SectionId = (typeof SECTIONS)[number]["id"];

const SIDEBAR_WIDTH_KEY = "phantom.settingsSidebarWidth";
const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 400;
const SIDEBAR_DEFAULT = 220;

let profileSeq = 0;
const newProfileId = () => `profile-${Date.now().toString(36)}-${(profileSeq++).toString(36)}`;

export function SettingsView({ config, onChange }: Props) {
  const [section, setSection] = useState<SectionId>("appearance");
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const saved = Number(localStorage.getItem(SIDEBAR_WIDTH_KEY));
    return saved >= SIDEBAR_MIN && saved <= SIDEBAR_MAX ? saved : SIDEBAR_DEFAULT;
  });
  const containerRef = useRef<HTMLDivElement>(null);

  // Drag the divider to resize the sidebar. We listen on the window during the
  // drag so the pointer can leave the handle without dropping the gesture.
  const startResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const container = containerRef.current;
    if (!container) return;
    const left = container.getBoundingClientRect().left;

    const onMove = (ev: MouseEvent) => {
      const next = Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, ev.clientX - left));
      setSidebarWidth(next);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, []);

  useEffect(() => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(sidebarWidth));
  }, [sidebarWidth]);

  return (
    <div ref={containerRef} className="flex h-full bg-[#0b0b0e] text-sm text-white/90">
      {/* Left sidebar: vertical section tabs. */}
      <nav
        className="flex shrink-0 flex-col gap-0.5 overflow-y-auto border-white/10 border-r bg-[#111116] p-2"
        style={{ width: sidebarWidth }}
      >
        <h2 className="px-2 pt-1 pb-2 font-semibold text-white/40 text-xs uppercase tracking-wide">
          Settings
        </h2>
        {SECTIONS.map(({ id, label, icon: Icon }) => {
          const active = id === section;
          return (
            <button
              key={id}
              type="button"
              onClick={() => setSection(id)}
              className={[
                "flex items-center gap-2.5 rounded px-2.5 py-2 text-left",
                active
                  ? "bg-white/15 text-white"
                  : "text-white/55 hover:bg-white/8 hover:text-white/85",
              ].join(" ")}
            >
              <Icon size={15} className="shrink-0" />
              <span className="truncate">{label}</span>
            </button>
          );
        })}
      </nav>

      {/* Drag handle between sidebar and content. Arrow keys nudge it too. */}
      {/* biome-ignore lint/a11y/useSemanticElements: a focusable window splitter is the standard ARIA pattern, not an <hr>. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize sidebar"
        aria-valuenow={Math.round(sidebarWidth)}
        aria-valuemin={SIDEBAR_MIN}
        aria-valuemax={SIDEBAR_MAX}
        tabIndex={0}
        onMouseDown={startResize}
        onKeyDown={(e) => {
          if (e.key === "ArrowLeft") {
            e.preventDefault();
            setSidebarWidth((w) => Math.max(SIDEBAR_MIN, w - 16));
          } else if (e.key === "ArrowRight") {
            e.preventDefault();
            setSidebarWidth((w) => Math.min(SIDEBAR_MAX, w + 16));
          }
        }}
        className="group relative w-1 shrink-0 cursor-col-resize outline-none"
      >
        <div className="absolute inset-y-0 left-0 w-px bg-white/10 group-hover:bg-sky-400/60 group-focus:bg-sky-400/60" />
      </div>

      {/* Right: the active section's content. */}
      <div className="min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-2xl px-8 py-6">
          {section === "appearance" && <AppearanceSection config={config} onChange={onChange} />}
          {section === "theme" && <ThemeSection config={config} onChange={onChange} />}
          {section === "profiles" && <ProfilesSection config={config} onChange={onChange} />}
          {section === "session" && <SessionSection config={config} onChange={onChange} />}
        </div>
      </div>
    </div>
  );
}

function AppearanceSection({ config, onChange }: Props) {
  return (
    <Section title="Appearance" hint="Font & theme changes apply live across all tabs.">
      <Field label="Font family">
        <input
          type="text"
          value={config.font_family}
          spellCheck={false}
          className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 outline-none focus:border-white/30"
          onChange={(e) => onChange({ font_family: e.target.value })}
        />
      </Field>

      <Field label={`Font size (${config.font_size}px)`}>
        <input
          type="range"
          min={8}
          max={32}
          value={config.font_size}
          className="w-full accent-white/80"
          onChange={(e) => onChange({ font_size: Number(e.target.value) })}
        />
      </Field>

      <Field label={`Line height (${config.line_height.toFixed(2)})`}>
        <input
          type="range"
          min={1}
          max={2.5}
          step={0.05}
          value={config.line_height}
          className="w-full accent-white/80"
          onChange={(e) => onChange({ line_height: Number(e.target.value) })}
        />
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="Cursor style">
          <CustomDropdown
            value={config.cursor_style}
            options={[
              { value: "block", label: "Block" },
              { value: "bar", label: "Bar" },
              { value: "underline", label: "Underline" },
            ]}
            onChange={(value) => onChange({ cursor_style: value as AppConfig["cursor_style"] })}
          />
        </Field>
        <Field label="Cursor">
          <label className="flex h-8 cursor-pointer items-center gap-2 rounded border border-white/10 bg-black/20 px-2 text-white/70">
            <input
              type="checkbox"
              checked={config.cursor_blink}
              className="accent-white/80"
              onChange={(e) => onChange({ cursor_blink: e.target.checked })}
            />
            <span>Blink</span>
          </label>
        </Field>
      </div>
    </Section>
  );
}

function ThemeSection({ config, onChange }: Props) {
  const setTheme = (patch: Partial<Theme>) => onChange({ theme: { ...config.theme, ...patch } });
  return (
    <Section title="Theme" hint="Colors apply live across all tabs.">
      <div className="grid grid-cols-2 gap-3">
        <Field label="Background">
          <ColorInput
            value={config.theme.background}
            onChange={(v) => setTheme({ background: v })}
          />
        </Field>
        <Field label="Foreground">
          <ColorInput
            value={config.theme.foreground}
            onChange={(v) => setTheme({ foreground: v })}
          />
        </Field>
        {COLOR_FIELDS.map(({ key, label }) => (
          <Field key={key} label={label}>
            <ColorInput
              value={config.theme[key]}
              onChange={(v) => setTheme({ [key]: v } as Partial<Theme>)}
            />
          </Field>
        ))}
        <Field label="Selection (rgba)">
          <input
            type="text"
            value={config.theme.selection}
            spellCheck={false}
            className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none focus:border-white/30"
            onChange={(e) => setTheme({ selection: e.target.value })}
          />
        </Field>
      </div>
    </Section>
  );
}

function ProfilesSection({ config, onChange }: Props) {
  const updateProfile = (id: string, patch: Partial<ShellProfile>) =>
    onChange({
      shell_profiles: config.shell_profiles.map((p) => (p.id === id ? { ...p, ...patch } : p)),
    });

  const addProfile = () =>
    onChange({
      shell_profiles: [
        ...config.shell_profiles,
        { id: newProfileId(), name: "New Profile", command: "", args: [], cwd: null },
      ],
    });

  const removeProfile = (id: string) => {
    const remaining = config.shell_profiles.filter((p) => p.id !== id);
    const patch: Partial<AppConfig> = { shell_profiles: remaining };
    if (config.default_shell_profile_id === id && remaining[0]) {
      patch.default_shell_profile_id = remaining[0].id;
    }
    onChange(patch);
  };

  return (
    <Section title="Shell Profiles" hint="Profile changes affect new tabs only.">
      <Field label="Default profile (used for new tabs)">
        <CustomDropdown
          value={config.default_shell_profile_id}
          options={config.shell_profiles.map((p) => ({ value: p.id, label: p.name }))}
          onChange={(v) => onChange({ default_shell_profile_id: v })}
        />
      </Field>

      <div className="space-y-3">
        {config.shell_profiles.map((p) => (
          <div key={p.id} className="rounded border border-white/10 bg-black/20 p-3">
            <div className="mb-2 flex items-center gap-2">
              <input
                type="text"
                value={p.name}
                spellCheck={false}
                className="flex-1 rounded border border-white/10 bg-black/30 px-2 py-1 font-medium outline-none focus:border-white/30"
                onChange={(e) => updateProfile(p.id, { name: e.target.value })}
              />
              <button
                type="button"
                aria-label="Delete profile"
                title="Delete profile"
                disabled={config.shell_profiles.length <= 1}
                className="flex h-7 w-7 items-center justify-center rounded text-white/50 hover:bg-red-500/30 hover:text-white disabled:cursor-not-allowed disabled:opacity-30"
                onClick={() => removeProfile(p.id)}
              >
                <Trash2 size={14} />
              </button>
            </div>
            <div className="grid grid-cols-2 gap-2">
              <Field label="Command">
                <input
                  type="text"
                  value={p.command}
                  placeholder="default ($SHELL)"
                  spellCheck={false}
                  className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none placeholder:text-white/30 focus:border-white/30"
                  onChange={(e) => updateProfile(p.id, { command: e.target.value })}
                />
              </Field>
              <Field label="Arguments">
                <ArgsList args={p.args} onChange={(args) => updateProfile(p.id, { args })} />
              </Field>
              <Field label="Working directory (optional)">
                <input
                  type="text"
                  value={p.cwd ?? ""}
                  placeholder="inherit / last cwd"
                  spellCheck={false}
                  className="col-span-2 w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none placeholder:text-white/30 focus:border-white/30"
                  onChange={(e) => updateProfile(p.id, { cwd: e.target.value.trim() || null })}
                />
              </Field>
            </div>
          </div>
        ))}
        <button
          type="button"
          className="flex items-center gap-2 rounded border border-white/15 border-dashed px-3 py-1.5 text-white/60 hover:border-white/30 hover:text-white"
          onClick={addProfile}
        >
          <Plus size={14} /> Add profile
        </button>
      </div>
    </Section>
  );
}

function ArgsList({ args, onChange }: { args: string[]; onChange: (args: string[]) => void }) {
  const updateArg = (index: number, value: string) => {
    onChange(args.map((arg, i) => (i === index ? value : arg)));
  };
  const removeArg = (index: number) => {
    onChange(args.filter((_, i) => i !== index));
  };
  return (
    <div className="space-y-1.5">
      {args.length === 0 ? (
        <div className="rounded border border-white/10 border-dashed px-2 py-1.5 text-white/30 text-xs">
          No arguments
        </div>
      ) : (
        args.map((arg, index) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: shell args are an ordered vector and duplicate values are valid.
          <div key={index} className="flex items-center gap-1.5">
            <input
              type="text"
              value={arg}
              aria-label={`Argument ${index + 1}`}
              spellCheck={false}
              className="min-w-0 flex-1 rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none focus:border-white/30"
              onChange={(e) => updateArg(index, e.target.value)}
            />
            <button
              type="button"
              aria-label={`Remove argument ${index + 1}`}
              title="Remove argument"
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded text-white/50 hover:bg-white/10 hover:text-white"
              onClick={() => removeArg(index)}
            >
              <X size={13} />
            </button>
          </div>
        ))
      )}
      <button
        type="button"
        className="flex items-center gap-1.5 rounded border border-white/15 border-dashed px-2 py-1 text-white/55 text-xs hover:border-white/30 hover:text-white"
        onClick={() => onChange([...args, ""])}
      >
        <Plus size={12} /> Add argument
      </button>
    </div>
  );
}

function SessionSection({ config, onChange }: Props) {
  return (
    <Section title="Session">
      <label className="flex cursor-pointer items-center gap-2">
        <input
          type="checkbox"
          checked={config.restore_on_launch}
          className="accent-white/80"
          onChange={(e) => onChange({ restore_on_launch: e.target.checked })}
        />
        <span>Restore tabs &amp; working directories on launch</span>
      </label>

      <Field label="Scrollback (lines kept in memory)">
        <input
          type="number"
          min={0}
          max={1_000_000}
          step={1000}
          value={config.scrollback_lines}
          className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 outline-none focus:border-white/30"
          onChange={(e) =>
            onChange({
              scrollback_lines: Math.max(0, Math.floor(Number(e.target.value)) || 0),
            })
          }
        />
        <span className="mt-1 block text-white/30 text-xs">
          In-memory only — terminal output is never written to disk.
        </span>
      </Field>
    </Section>
  );
}

function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-4">
      <header>
        <h3 className="font-medium text-base text-white">{title}</h3>
        {hint && <p className="mt-0.5 text-white/35 text-xs">{hint}</p>}
      </header>
      <div className="space-y-3">{children}</div>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  // A plain wrapper (not <label>) because some fields contain custom controls
  // (e.g. the profile dropdown) rather than a single native form element.
  return (
    <div className="block">
      <span className="mb-1 block text-white/40 text-xs">{label}</span>
      {children}
    </div>
  );
}

function ColorInput({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return (
    <div className="flex items-center gap-2">
      <input
        type="color"
        value={value.slice(0, 7)}
        className="h-8 w-9 shrink-0 cursor-pointer rounded border border-white/10 bg-transparent"
        onChange={(e) => onChange(e.target.value)}
      />
      <input
        type="text"
        value={value}
        spellCheck={false}
        className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none focus:border-white/30"
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}
