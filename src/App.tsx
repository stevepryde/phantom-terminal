import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "react";
import { CommandPalette } from "./command-palette/CommandPalette";
import {
  type AppConfig,
  configGet,
  configSet,
  homeDir,
  isTauri,
  ptyCwd,
  type TabRecord,
  tabsLoad,
  tabsSave,
} from "./lib/ipc";
import { setHomeDir } from "./lib/paths";
import { useStore } from "./lib/store";
import { SettingsView } from "./settings/SettingsView";
import { TabSessionSaver } from "./store/tabPersistence";
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

const isMac = /mac/i.test(navigator.platform || navigator.userAgent);

export default function App() {
  const tabs = useStore(tabsStore, (s) => s.tabs);
  const activeId = useStore(tabsStore, (s) => s.activeId);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [chromePaintRevision, setChromePaintRevision] = useState(0);
  const [windowMaximized, setWindowMaximized] = useState(false);
  const readyRef = useRef(false);
  const homePathRef = useRef<string | null>(null);
  const sessionSaverRef = useRef<TabSessionSaver | null>(null);
  // Mirror config into a ref so the (deps-free) keyboard/command handlers can
  // read the current default profile without re-binding listeners.
  const configRef = useRef<AppConfig | null>(null);
  configRef.current = config;

  // The live cwd of a tab: query the PTY (authoritative) and fall back to the
  // last-known stored cwd.
  async function liveCwd(tab: Tab): Promise<string | null> {
    if (tab.ptyId != null) {
      const live = await ptyCwd(tab.ptyId);
      if (live) return live;
    }
    return tab.cwd;
  }

  sessionSaverRef.current ??= new TabSessionSaver({
    getSnapshot: () => tabsStore.state,
    resolveCwd: liveCwd,
    saveTabs: tabsSave,
    onCwdResolved: setTabCwd,
    onError: (err) => {
      console.error("phantom: failed to save session", err);
    },
  });

  function requestSessionSave() {
    if (!readyRef.current) return;
    sessionSaverRef.current?.requestSave();
  }

  async function saveSessionNow() {
    if (!readyRef.current) return;
    await sessionSaverRef.current?.saveNow();
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
  // biome-ignore lint/correctness/useExhaustiveDependencies: launch bootstrap runs once; session saver reads live store refs.
  useEffect(() => {
    (async () => {
      const cfg = await configGet();
      setConfig(cfg);
      const home = await homeDir().catch(() => null);
      homePathRef.current = home;
      setHomeDir(home);

      if (tabsStore.state.tabs.length) {
        readyRef.current = true;
        requestSessionSave();
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
      sessionSaverRef.current?.remember(restored);

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
      readyRef.current = true;
      requestSessionSave();
    })();
  }, []);

  // Save on tab add/close/rename/activate (debounced) + periodically + on blur.
  // biome-ignore lint/correctness/useExhaustiveDependencies: subscribes once for the app's lifetime; the session saver reads live state from the store.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const sub = tabsStore.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(requestSessionSave, 500);
    });
    const interval = setInterval(requestSessionSave, 10_000);
    const onBlur = () => {
      void saveSessionNow();
    };
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

  // Moving a WKWebView between macOS displays can leave custom draggable chrome
  // in a stale paint state until the next resize. Nudge chrome + terminals after
  // display-affecting window events so crossing screens behaves like a resize.
  useEffect(() => {
    if (!isTauri()) return;

    const win = getCurrentWindow();
    let timers: Array<ReturnType<typeof setTimeout>> = [];
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const clearRefreshTimers = () => {
      for (const timer of timers) clearTimeout(timer);
      timers = [];
    };

    const emitDisplayLayoutRefresh = () => {
      if (disposed) return;
      setChromePaintRevision((revision) => revision + 1);
      window.dispatchEvent(new Event("phantom:display-layout-change"));
    };

    const refreshDisplayLayout = () => {
      clearRefreshTimers();
      timers = [0, 50, 150, 300].map((delay) => setTimeout(emitDisplayLayoutRefresh, delay));
    };

    void Promise.all([
      win.onScaleChanged(refreshDisplayLayout),
      win.onMoved(refreshDisplayLayout),
      win.onResized(refreshDisplayLayout),
      win.onFocusChanged(({ payload: focused }) => {
        if (focused) refreshDisplayLayout();
      }),
    ]).then((listeners) => {
      if (disposed) {
        for (const unlisten of listeners) unlisten();
      } else {
        unlisteners.push(...listeners);
      }
    });

    return () => {
      disposed = true;
      clearRefreshTimers();
      for (const unlisten of unlisteners) unlisten();
    };
  }, []);

  // Rounded transparent windows should square off when maximized/fullscreen so
  // the content still reaches the display edges.
  useEffect(() => {
    if (!isTauri()) return;

    const win = getCurrentWindow();
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const updateWindowShape = () => {
      void Promise.all([win.isMaximized(), win.isFullscreen()])
        .then(([maximized, fullscreen]) => {
          if (!disposed) setWindowMaximized(maximized || fullscreen);
        })
        .catch((err) => {
          console.error("phantom: failed to read window shape state", err);
        });
    };

    updateWindowShape();

    void Promise.all([
      win.onResized(updateWindowShape),
      win.onMoved(updateWindowShape),
      win.onFocusChanged(updateWindowShape),
    ]).then((listeners) => {
      if (disposed) {
        for (const unlisten of listeners) unlisten();
      } else {
        unlisteners.push(...listeners);
      }
    });

    return () => {
      disposed = true;
      for (const unlisten of unlisteners) unlisten();
    };
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

function defaultLaunchCwd(config: AppConfig | null, home: string | null): string | null {
  if (!config) return home;
  const profile =
    config.shell_profiles.find((p) => p.id === config.default_shell_profile_id) ??
    config.shell_profiles[0];
  return profile?.cwd?.trim() || home;
}
