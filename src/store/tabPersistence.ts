import type { TabRecord } from "../lib/ipc";
import { normalizeCwd, type Tab } from "./tabs";

export interface TabSnapshot {
  tabs: Tab[];
  activeId: string | null;
}

export interface TabSessionSaverDeps {
  getSnapshot: () => TabSnapshot;
  resolveCwd: (tab: Tab) => Promise<string | null>;
  saveTabs: (records: TabRecord[]) => Promise<void>;
  onCwdResolved: (tabId: string, cwd: string) => void;
  onError?: (error: unknown) => void;
}

export class TabSessionSaver {
  private previousRecords = new Map<string, TabRecord>();
  private inFlight = false;
  private queued = false;
  private idleWaiters: Array<() => void> = [];

  constructor(private readonly deps: TabSessionSaverDeps) {}

  remember(records: TabRecord[]) {
    this.previousRecords = recordsById(records);
  }

  requestSave() {
    this.queued = true;
    void this.drain();
  }

  async saveNow() {
    this.queued = true;
    void this.drain();
    await this.waitForIdle();
  }

  private async drain() {
    if (this.inFlight) return;
    this.inFlight = true;

    try {
      while (this.queued) {
        this.queued = false;
        await this.saveCurrentSnapshot();
      }
    } finally {
      this.inFlight = false;
      if (this.queued) {
        void this.drain();
      } else {
        this.resolveIdleWaiters();
      }
    }
  }

  private async saveCurrentSnapshot() {
    try {
      const { records, cwdUpdates } = await buildTabRecords(
        this.deps.getSnapshot(),
        this.deps.resolveCwd,
        this.previousRecords,
      );

      for (const update of cwdUpdates) {
        this.deps.onCwdResolved(update.tabId, update.cwd);
      }

      if (records.length === 0 || this.queued) return;

      await this.deps.saveTabs(records);
      this.previousRecords = recordsById(records);
    } catch (error) {
      this.deps.onError?.(error);
    }
  }

  private waitForIdle(): Promise<void> {
    if (!this.inFlight && !this.queued) return Promise.resolve();
    return new Promise((resolve) => {
      this.idleWaiters.push(resolve);
    });
  }

  private resolveIdleWaiters() {
    const waiters = this.idleWaiters;
    this.idleWaiters = [];
    for (const resolve of waiters) resolve();
  }
}

export async function buildTabRecords(
  snapshot: TabSnapshot,
  resolveCwd: (tab: Tab) => Promise<string | null>,
  previousRecords = new Map<string, TabRecord>(),
): Promise<{ records: TabRecord[]; cwdUpdates: Array<{ tabId: string; cwd: string }> }> {
  const records: TabRecord[] = [];
  const cwdUpdates: Array<{ tabId: string; cwd: string }> = [];

  for (const tab of snapshot.tabs) {
    if (tab.kind === "settings") continue;

    const resolved = normalizeCwd(await resolveCwd(tab));
    const previous = previousRecords.get(tab.id);
    const cwd = resolved ?? tab.cwd ?? normalizeCwd(previous?.cwd) ?? "";

    if (resolved && resolved !== tab.cwd) {
      cwdUpdates.push({ tabId: tab.id, cwd: resolved });
    }

    records.push({
      id: tab.id,
      title: tab.customTitle ?? "",
      cwd,
      is_active: tab.id === snapshot.activeId,
      shell_profile_id: tab.shellProfileId,
      created_at: tab.createdAt,
      updated_at: tab.updatedAt,
    });
  }

  return { records, cwdUpdates };
}

function recordsById(records: TabRecord[]): Map<string, TabRecord> {
  const byId = new Map<string, TabRecord>();
  for (const record of records) {
    if (record.id) byId.set(record.id, record);
  }
  return byId;
}
