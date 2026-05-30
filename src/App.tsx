import { useEffect, useRef, useState } from "react";
import { CommandPalette } from "./command-palette/CommandPalette";
import { useAppConfig } from "./hooks/useAppConfig";
import { useDisplayLayoutRefresh } from "./hooks/useDisplayLayoutRefresh";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useLiveCwdPolling } from "./hooks/useLiveCwdPolling";
import { useSessionPersistence } from "./hooks/useSessionPersistence";
import { useWindowShape } from "./hooks/useWindowShape";
import {
  type AppConfig,
  configGet,
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
  replaceTabs,
  setTabCwd,
  setTabPty,
  type Tab,
  tabsStore,
  toggleSettingsTab,
} from "./store/tabs";
import { TabBar, TitleBarChrome } from "./tabs/TabBar";
import { TerminalView } from "./terminal/TerminalView";

export default function App() {
  const tabs = useStore(tabsStore, (s) => s.tabs);
  const activeId = useStore(tabsStore, (s) => s.activeId);
  const { config, configError, configRef, initConfig, updateConfig } = useAppConfig();
  const [editingId, setEditingId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const homePathRef = useRef<string | null>(null);

  const chromePaintRevision = useDisplayLayoutRefresh();
  const windowMaximized = useWindowShape();
  useLiveCwdPolling();

  // The session saver reads the store's `tab.cwd` (kept current by the cwd
  // poller above) rather than resolving cwd again — one cwd-resolution path.
  const persistence = useSessionPersistence({
    getSnapshot: () => tabsStore.state,
    resolveCwd: (tab: Tab) => Promise.resolve(tab.cwd),
    saveTabs: tabsSave,
    onCwdResolved: setTabCwd,
    onError: (err) => {
      console.error("phantom: failed to save session", err);
    },
  });

  // The live cwd of a tab: query the PTY (authoritative) and fall back to the
  // last-known stored cwd. Used when opening a tab so it inherits the freshest
  // directory even between cwd-poll ticks.
  async function liveCwd(tab: Tab): Promise<string | null> {
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
    const cwd =
      (source ? await liveCwd(source) : null) ??
      source?.cwd ??
      defaultLaunchCwd(configRef.current, homePathRef.current);
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

  function closeCommandPalette() {
    setPaletteOpen(false);
    requestAnimationFrame(() => {
      window.dispatchEvent(new Event("phantom:focus-active-terminal"));
    });
  }

  useKeyboardShortcuts(config, {
    toggleSettings: toggleSettingsTab,
    togglePalette: () => setPaletteOpen((v) => !v),
    newTab,
    closeActiveTab: () => {
      const id = tabsStore.state.activeId;
      if (id) closeTab(id);
    },
    renameActiveTab: () => {
      const id = tabsStore.state.activeId;
      if (id) setEditingId(id);
    },
    activateRelative,
    activateIndex,
  });

  // Load config + restore saved session on launch.
  // biome-ignore lint/correctness/useExhaustiveDependencies: launch bootstrap runs once; the config + persistence controllers are stable.
  useEffect(() => {
    (async () => {
      const cfg = await configGet();
      initConfig(cfg);
      const home = await homeDir().catch(() => null);
      homePathRef.current = home;
      setHomeDir(home);

      if (tabsStore.state.tabs.length) {
        persistence.markReady();
        persistence.requestSave();
        return;
      }

      let restored: TabRecord[] = [];
      if (cfg.restore_on_launch) {
        try {
          restored = await tabsLoad();
        } catch (err) {
          console.error("phantom: failed to restore saved tabs", err);
          restored = [];
        }
      }
      persistence.remember(restored);

      if (restored.length) {
        replaceTabs(
          restored.map((r) => ({
            id: r.id ?? null,
            cwd: r.cwd,
            customTitle: r.title || null,
            shellProfileId: r.shell_profile_id ?? null,
            createdAt: r.created_at ?? null,
            updatedAt: r.updated_at ?? null,
            active: Boolean(r.is_active),
          })),
        );
      } else {
        replaceTabs([
          {
            cwd: defaultLaunchCwd(cfg, home),
            shellProfileId: cfg.default_shell_profile_id,
            active: true,
          },
        ]);
      }
      persistence.markReady();
      persistence.requestSave();
    })();
  }, []);

  if (!config) {
    return (
      <div
        className={`app-window grid place-items-center text-white/40 ${
          windowMaximized ? "app-window--maximized" : ""
        }`}
      >
        Loading…
      </div>
    );
  }

  const tabLayout = config.tab_layout;
  const tabBarProps = {
    paintRevision: chromePaintRevision,
    tabs,
    activeId,
    editingId,
    onActivate: activateTab,
    onClose: closeTab,
    onAdd: newTab,
    onNewTabAfter: newTabAfter,
    onStartRename: setEditingId,
    onCommitRename: (id: string, title: string) => {
      renameTab(id, title);
      setEditingId(null);
    },
    onCancelRename: () => setEditingId(null),
    onOpenSettings: openSettingsTab,
  };

  const tabContent = (
    <div className="relative min-h-0 flex-1 bg-[#0b0b0e]">
      {tabs.map((tab) => (
        // Every tab stays mounted (keeps its PTY alive) and is absolutely
        // stacked. Without this, an inactive tab later in the array overlays
        // the active terminal and swallows clicks — the terminal then can't be
        // focused, so you see a cursor but can't type. Only the active wrapper
        // accepts pointer events; the rest let clicks fall through.
        <div
          key={tab.id}
          // Terminal tabs keep a real pane inset on every side; the renderer
          // handles trimming any fake first-row blank from the emulator.
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
            <SettingsView config={config} error={configError} onChange={updateConfig} />
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
  );

  return (
    <div
      className={`app-window relative flex flex-col ${
        windowMaximized ? "app-window--maximized" : ""
      }`}
    >
      {tabLayout === "horizontal" ? (
        <>
          <TabBar layout={tabLayout} {...tabBarProps} />
          {tabContent}
        </>
      ) : (
        <>
          <TitleBarChrome paintRevision={chromePaintRevision} onOpenSettings={openSettingsTab} />
          <div className="flex min-h-0 flex-1">
            <TabBar layout={tabLayout} {...tabBarProps} />
            {tabContent}
          </div>
        </>
      )}
      {paletteOpen && (
        <CommandPalette
          tabs={tabs}
          activeId={activeId}
          onClose={closeCommandPalette}
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

function defaultLaunchCwd(config: AppConfig | null, home: string | null): string | null {
  if (!config) return home;
  const profile =
    config.shell_profiles.find((p) => p.id === config.default_shell_profile_id) ??
    config.shell_profiles[0];
  return profile?.cwd?.trim() || home;
}
