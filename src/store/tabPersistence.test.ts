import { expect, test } from "bun:test";
import type { TabRecord } from "../lib/ipc";
import { buildTabRecords, TabSessionSaver } from "./tabPersistence";
import type { Tab } from "./tabs";

const terminalTab = (patch: Partial<Tab> = {}): Tab => ({
  id: "tab-1",
  kind: "terminal",
  customTitle: null,
  cwd: "/current",
  ptyId: null,
  shellProfileId: null,
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
  ...patch,
});

test("buildTabRecords preserves previous cwd when current cwd is unknown", async () => {
  const previous = new Map<string, TabRecord>([
    [
      "tab-1",
      {
        id: "tab-1",
        title: "",
        cwd: "/kept",
        is_active: true,
      },
    ],
  ]);

  const { records } = await buildTabRecords(
    { tabs: [terminalTab({ cwd: null })], activeId: "tab-1" },
    async () => null,
    previous,
  );

  expect(records[0].cwd).toBe("/kept");
});

test("buildTabRecords reports authoritative live cwd updates", async () => {
  const { records, cwdUpdates } = await buildTabRecords(
    { tabs: [terminalTab({ cwd: "/old" })], activeId: "tab-1" },
    async () => "/new",
  );

  expect(records[0].cwd).toBe("/new");
  expect(cwdUpdates).toEqual([{ tabId: "tab-1", cwd: "/new" }]);
});

test("TabSessionSaver skips stale snapshots and saves the latest queued state", async () => {
  let snapshot = { tabs: [terminalTab({ cwd: null })], activeId: "tab-1" };
  let releaseFirst: (cwd: string | null) => void = () => {};
  let resolveCount = 0;
  const saved: TabRecord[][] = [];

  const saver = new TabSessionSaver({
    getSnapshot: () => snapshot,
    resolveCwd: async (tab) => {
      resolveCount += 1;
      if (resolveCount === 1) {
        return new Promise<string | null>((resolve) => {
          releaseFirst = resolve;
        });
      }
      return tab.cwd;
    },
    saveTabs: async (records) => {
      saved.push(records);
    },
    onCwdResolved: (tabId, cwd) => {
      snapshot = {
        ...snapshot,
        tabs: snapshot.tabs.map((tab) => (tab.id === tabId ? { ...tab, cwd } : tab)),
      };
    },
  });

  saver.requestSave();
  await Promise.resolve();
  snapshot = { tabs: [terminalTab({ cwd: "/latest" })], activeId: "tab-1" };
  saver.requestSave();
  releaseFirst(null);
  await saver.saveNow();

  expect(saved.length).toBeGreaterThanOrEqual(1);
  expect(saved.every((records) => records[0].cwd !== "")).toBe(true);
  expect(saved.at(-1)?.[0].cwd).toBe("/latest");
});
