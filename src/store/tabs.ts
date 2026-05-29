import { formatCwdName } from "../lib/paths";
import { createStore } from "../lib/store";

export interface Tab {
  id: string;
  /** Explicit user-given name. When null, the tab is named after its cwd. */
  customTitle: string | null;
  cwd: string;
  ptyId: number | null;
  shellProfileId: string | null;
}

/** The label shown for a tab: explicit name, else cwd-derived, else "shell". */
export function tabTitle(tab: Tab): string {
  return tab.customTitle?.trim() || formatCwdName(tab.cwd) || "shell";
}

interface TabsState {
  tabs: Tab[];
  activeId: string | null;
}

export const tabsStore = createStore<TabsState>({ tabs: [], activeId: null });

let counter = 0;
const newId = () => `${Date.now().toString(36)}-${(counter++).toString(36)}`;

export function addTab(
  opts: {
    cwd?: string;
    customTitle?: string | null;
    shellProfileId?: string | null;
    /** Insert the new tab immediately after this tab id; appends if omitted. */
    afterId?: string;
  } = {},
): string {
  const id = newId();
  const tab: Tab = {
    id,
    customTitle: opts.customTitle ?? null,
    cwd: opts.cwd ?? "",
    ptyId: null,
    shellProfileId: opts.shellProfileId ?? null,
  };
  tabsStore.setState((s) => {
    const idx = opts.afterId ? s.tabs.findIndex((t) => t.id === opts.afterId) : -1;
    if (idx >= 0) {
      const tabs = [...s.tabs.slice(0, idx + 1), tab, ...s.tabs.slice(idx + 1)];
      return { tabs, activeId: id };
    }
    return { tabs: [...s.tabs, tab], activeId: id };
  });
  return id;
}

export function closeTab(id: string) {
  tabsStore.setState((s) => {
    const idx = s.tabs.findIndex((t) => t.id === id);
    const tabs = s.tabs.filter((t) => t.id !== id);
    let activeId = s.activeId;
    if (activeId === id) {
      activeId = tabs.length ? tabs[Math.min(idx, tabs.length - 1)].id : null;
    }
    return { tabs, activeId };
  });
}

/**
 * Move `fromId` into the gap at `gapIndex`, where gaps are numbered 0..tabs.length
 * (gap i sits before tab i; gap tabs.length is the end). Reordering never changes
 * which tab is active.
 */
export function moveTab(fromId: string, gapIndex: number) {
  tabsStore.setState((s) => {
    const from = s.tabs.findIndex((t) => t.id === fromId);
    if (from < 0) return s;
    const tabs = [...s.tabs];
    const [moved] = tabs.splice(from, 1);
    // Removing an earlier element shifts every later gap left by one.
    let target = from < gapIndex ? gapIndex - 1 : gapIndex;
    target = Math.max(0, Math.min(target, tabs.length));
    if (target === from) return s; // no-op
    tabs.splice(target, 0, moved);
    return { ...s, tabs };
  });
}

export function activateTab(id: string) {
  tabsStore.setState((s) => (s.activeId === id ? s : { ...s, activeId: id }));
}

export function activateIndex(index: number) {
  const { tabs } = tabsStore.state;
  if (tabs[index]) activateTab(tabs[index].id);
}

export function activateRelative(delta: number) {
  const { tabs, activeId } = tabsStore.state;
  if (!tabs.length) return;
  const cur = tabs.findIndex((t) => t.id === activeId);
  const next = ((cur < 0 ? 0 : cur) + delta + tabs.length) % tabs.length;
  activateTab(tabs[next].id);
}

/** Set (or clear) a tab's explicit name. An empty name reverts to cwd-naming. */
export function renameTab(id: string, title: string) {
  const clean = title.trim();
  tabsStore.setState((s) => ({
    ...s,
    tabs: s.tabs.map((t) => (t.id === id ? { ...t, customTitle: clean || null } : t)),
  }));
}

export function setTabPty(id: string, ptyId: number) {
  tabsStore.setState((s) => ({
    ...s,
    tabs: s.tabs.map((t) => (t.id === id ? { ...t, ptyId } : t)),
  }));
}

export function setTabCwd(id: string, cwd: string) {
  tabsStore.setState((s) => {
    const tab = s.tabs.find((t) => t.id === id);
    if (!tab || tab.cwd === cwd) return s; // no-op when unchanged (avoids churn)
    return { ...s, tabs: s.tabs.map((t) => (t.id === id ? { ...t, cwd } : t)) };
  });
}
