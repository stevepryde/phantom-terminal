import { Plus, Trash2, X } from "lucide-react";
import { useEffect } from "react";
import { CustomDropdown } from "../components/CustomDropdown";
import type { AppConfig, ShellProfile, Theme } from "../lib/ipc";

interface Props {
  config: AppConfig;
  onChange: (patch: Partial<AppConfig>) => void;
  onClose: () => void;
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

let profileSeq = 0;
const newProfileId = () => `profile-${Date.now().toString(36)}-${(profileSeq++).toString(36)}`;

export function SettingsModal({ config, onChange, onClose }: Props) {
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const setTheme = (patch: Partial<Theme>) => onChange({ theme: { ...config.theme, ...patch } });

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
    <div
      className="absolute inset-0 z-50 grid place-items-center bg-black/50"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="flex max-h-[85vh] w-[34rem] max-w-[92vw] flex-col rounded-lg border border-white/10 bg-[#16161c] text-sm text-white/90 shadow-2xl">
        <div className="flex items-center justify-between border-white/10 border-b px-5 py-3">
          <h2 className="font-medium text-base">Settings</h2>
          <button
            type="button"
            aria-label="Close"
            className="flex h-6 w-6 items-center justify-center rounded text-white/50 hover:bg-white/10 hover:text-white"
            title="Close (Esc)"
            onClick={onClose}
          >
            <X size={16} />
          </button>
        </div>

        <div className="space-y-6 overflow-y-auto px-5 py-4">
          <Section title="Appearance">
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
          </Section>

          <Section title="Theme">
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

          <Section title="Shell profiles">
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
                    <Field label="Args (space-separated)">
                      <input
                        type="text"
                        value={p.args.join(" ")}
                        spellCheck={false}
                        className="w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none focus:border-white/30"
                        onChange={(e) =>
                          updateProfile(p.id, {
                            args: e.target.value.split(/\s+/).filter(Boolean),
                          })
                        }
                      />
                    </Field>
                    <Field label="Working directory (optional)">
                      <input
                        type="text"
                        value={p.cwd ?? ""}
                        placeholder="inherit / last cwd"
                        spellCheck={false}
                        className="col-span-2 w-full rounded border border-white/10 bg-black/30 px-2 py-1 font-mono text-xs outline-none placeholder:text-white/30 focus:border-white/30"
                        onChange={(e) =>
                          updateProfile(p.id, { cwd: e.target.value.trim() || null })
                        }
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
        </div>

        <p className="border-white/10 border-t px-5 py-3 text-white/35 text-xs">
          Font &amp; theme changes apply live. Shell profile changes affect new tabs only.
        </p>
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3">
      <h3 className="font-semibold text-white/50 text-xs uppercase tracking-wide">{title}</h3>
      {children}
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
