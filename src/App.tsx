import { useEffect, useRef, useState } from "react";
import { CommandPalette } from "./command-palette/CommandPalette";
import {
  type AppConfig,
  configGet,
  configSet,
  homeDir,
  ptyCwd,
  type TabRecord,
  tabsLoad,
  tabsSave,
} from "./lib/ipc";
import { setHomeDir } from "./lib/paths";
import { useStore } from "./lib/store";
import { SettingsView } from "./settings/SettingsView";
import {
  activateIndex,
  activateRelative,
  activateTab,
  addTab,
  closeTab,
  openSettingsTab,
  renameTab,
  setTabCwd,
  setTabPty,
  type Tab,
  tabsStore,
  toggleSettingsTab,
} from "./store/tabs";
import { TabBar } from "./tabs/TabBar";
import { TerminalView } from "./terminal/TerminalView";

const isMac = /mac/i.test(navigator.platform || navigator.userAgent);

export default function App() {
  const tabs = useStore(tabsStore, (s) => s.tabs);
  const activeId = useStore(tabsStore, (s) => s.activeId);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const readyRef = useRef(false);
  // Mirror config into a ref so the (deps-free) keyboard/command handlers can
  // read the current default profile without re-binding listeners.
  const configRef = useRef<AppConfig | null>(null);
  configRef.current = config;

  // The live cwd of a tab: query the PTY (authoritative) and fall back to the
  // last-known stored cwd.
  async function liveCwd(tab: Tab): Promise<string> {
    if (tab.ptyId != null) {
      const live = await ptyCwd(tab.ptyId);
      if (live) return live;
    }
    return tab.cwd;
  }

  // Open a new tab that inherits `fromId`'s cwd. When `afterId` is given the tab
  // is inserted just to its right; otherwise it is appended at the end.
  async function openTab(fromId: string | null, afterId?: string) {
    const source = tabsStore.state.tabs.find((t) => t.id === fromId);
    const cwd = source ? await liveCwd(source) : "";
    addTab({
      cwd,
      afterId,
      shellProfileId: configRef.current?.default_shell_profile_id ?? null,
    });
  }

  // Default new tab: inherit the active tab's cwd, append at the end.
  const newTab = () => openTab(tabsStore.state.activeId);
  // Context-menu new tab: inherit the clicked tab's cwd, insert to its right.
  const newTabAfter = (id: string) => openTab(id, id);

  // Apply a config patch live and persist it (best-effort).
  function updateConfig(patch: Partial<AppConfig>) {
    setConfig((prev) => {
      if (!prev) return prev;
      const next = { ...prev, ...patch };
      void configSet(next).catch((err) => {
        console.error("phantom: failed to persist config", err);
      });
      return next;
    });
  }

  // Load config + restore saved session on launch.
  useEffect(() => {
    (async () => {
      const cfg = await configGet();
      setConfig(cfg);
      setHomeDir(await homeDir().catch(() => null));

      let restored: TabRecord[] = [];
      if (cfg.restore_on_launch) {
        try {
          restored = await tabsLoad();
        } catch (err) {
          console.error("phantom: failed to restore saved tabs", err);
          restored = [];
        }
      }

      if (restored.length) {
        let activate: string | null = null;
        for (const r of restored) {
          const id = addTab({
            id: r.id ?? null,
            cwd: r.cwd,
            customTitle: r.title || null,
            shellProfileId: r.shell_profile_id ?? null,
            createdAt: r.created_at ?? null,
            updatedAt: r.updated_at ?? null,
          });
          if (r.is_active) activate = id;
        }
        if (activate) activateTab(activate);
      } else {
        addTab({ shellProfileId: cfg.default_shell_profile_id });
      }
      readyRef.current = true;
    })();
  }, []);

  // Persist session: gather live cwd per tab and save.
  async function saveSession() {
    if (!readyRef.current) return;
    const { tabs, activeId } = tabsStore.state;
    const records: TabRecord[] = [];
    for (const t of tabs) {
      if (t.kind === "settings") continue; // the settings tab is never persisted
      let cwd = t.cwd;
      if (t.ptyId != null) {
        const live = await ptyCwd(t.ptyId);
        if (live) {
          cwd = live;
          if (live !== t.cwd) setTabCwd(t.id, live);
        }
      }
      records.push({
        id: t.id,
        title: t.customTitle ?? "",
        cwd,
        is_active: t.id === activeId,
        shell_profile_id: t.shellProfileId,
        created_at: t.createdAt,
        updated_at: t.updatedAt,
      });
    }
    try {
      await tabsSave(records);
    } catch (err) {
      console.error("phantom: failed to save session", err);
    }
  }

  // Save on tab add/close/rename/activate (debounced) + periodically + on blur.
  // biome-ignore lint/correctness/useExhaustiveDependencies: subscribes once for the app's lifetime; saveSession reads live state from the store.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const sub = tabsStore.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(saveSession, 500);
    });
    const interval = setInterval(saveSession, 10_000);
    const onBlur = () => saveSession();
    window.addEventListener("blur", onBlur);
    return () => {
      sub();
      clearTimeout(timer);
      clearInterval(interval);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  // Keyboard shortcuts.
  // biome-ignore lint/correctness/useExhaustiveDependencies: listeners bound once; handlers read live state from refs/store and the stable newTab closure.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const primary = isMac ? e.metaKey : e.ctrlKey && e.shiftKey;
      const k = e.key.toLowerCase();

      if ((isMac ? e.metaKey : e.ctrlKey) && k === ",") {
        e.preventDefault();
        toggleSettingsTab();
        return;
      }
      if ((isMac ? e.metaKey : e.ctrlKey) && k === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
        return;
      }
      if (primary && k === "t") {
        e.preventDefault();
        newTab();
        return;
      }
      if (primary && k === "w") {
        e.preventDefault();
        const id = tabsStore.state.activeId;
        if (id) closeTab(id);
        return;
      }
      if (e.ctrlKey && e.code === "Tab") {
        e.preventDefault();
        activateRelative(e.shiftKey ? -1 : 1);
        return;
      }
      if (isMac && e.metaKey && e.shiftKey && e.code === "BracketLeft") {
        e.preventDefault();
        activateRelative(-1);
        return;
      }
      if (isMac && e.metaKey && e.shiftKey && e.code === "BracketRight") {
        e.preventDefault();
        activateRelative(1);
        return;
      }
      const numMod = isMac ? e.metaKey : e.altKey;
      if (numMod && /^Digit[1-9]$/.test(e.code)) {
        e.preventDefault();
        activateIndex(Number(e.code.slice(5)) - 1);
        return;
      }
      if (e.key === "F2") {
        const id = tabsStore.state.activeId;
        if (id) {
          e.preventDefault();
          setEditingId(id);
        }
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // Poll live cwd so cwd-named tabs track `cd`. setTabCwd is a no-op when the
  // path is unchanged, so this only mutates the store on a real directory change.
  useEffect(() => {
    const id = setInterval(async () => {
      for (const t of tabsStore.state.tabs) {
        if (t.ptyId == null) continue;
        const live = await ptyCwd(t.ptyId);
        if (live) setTabCwd(t.id, live);
      }
    }, 1500);
    return () => clearInterval(id);
  }, []);

  if (!config) {
    return <div className="grid h-full place-items-center text-white/40">Loading…</div>;
  }

  return (
    <div className="relative flex h-full flex-col">
      <TabBar
        tabs={tabs}
        activeId={activeId}
        editingId={editingId}
        onActivate={activateTab}
        onClose={closeTab}
        onAdd={newTab}
        onNewTabRight={newTabAfter}
        onStartRename={setEditingId}
        onCommitRename={(id, title) => {
          renameTab(id, title);
          setEditingId(null);
        }}
        onCancelRename={() => setEditingId(null)}
        onOpenSettings={openSettingsTab}
      />
      <div className="relative min-h-0 flex-1 bg-[#0b0b0e]">
        {tabs.map((tab) => (
          // Every tab stays mounted (keeps its PTY alive) and is absolutely
          // stacked. Without this, an inactive tab later in the array overlays
          // the active terminal and swallows clicks — the terminal then can't be
          // focused, so you see a cursor but can't type. Only the active wrapper
          // accepts pointer events; the rest let clicks fall through.
          <div
            key={tab.id}
            // Terminal tabs get a small inset so the pane doesn't sit flush
            // against the window edges; the settings tab manages its own layout.
            className={`absolute inset-0 ${tab.kind === "settings" ? "" : "p-2"}`}
            style={{
              pointerEvents: tab.id === activeId ? "auto" : "none",
              // TerminalView hides itself when inactive, but SettingsView does
              // not — so an inactive settings tab would keep painting on top of
              // the active terminal. Hide its wrapper when it isn't active.
              display: tab.kind === "settings" && tab.id !== activeId ? "none" : undefined,
            }}
          >
            {tab.kind === "settings" ? (
              <SettingsView config={config} onChange={updateConfig} />
            ) : (
              <TerminalView
                tabId={tab.id}
                cwd={tab.cwd}
                active={tab.id === activeId}
                config={config}
                shellProfileId={tab.shellProfileId}
                onSpawn={setTabPty}
              />
            )}
          </div>
        ))}
      </div>
      {paletteOpen && (
        <CommandPalette
          tabs={tabs}
          activeId={activeId}
          onClose={() => setPaletteOpen(false)}
          onSelectTab={activateTab}
          onNewTab={newTab}
          onCloseTab={closeTab}
          onRenameTab={setEditingId}
          onOpenSettings={openSettingsTab}
        />
      )}
    </div>
  );
}
