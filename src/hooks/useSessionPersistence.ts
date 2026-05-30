import { useCallback, useEffect, useRef } from "react";
import type { TabRecord } from "../lib/ipc";
import { TabSessionSaver, type TabSessionSaverDeps } from "../store/tabPersistence";
import { tabsStore } from "../store/tabs";

export interface SessionPersistence {
  /** Debounced save; ignored until `markReady` has been called. */
  requestSave: () => void;
  /** Flush a save and await completion; ignored until `markReady`. */
  saveNow: () => Promise<void>;
  /** Seed the saver with the records loaded at launch (for cwd carry-over). */
  remember: (records: TabRecord[]) => void;
  /** Allow saves to run — called once launch bootstrap has populated tabs. */
  markReady: () => void;
}

/**
 * Own the session saver and its triggers: a debounced save on every store
 * change, a periodic save, and a save on window blur. Saves are gated behind
 * `markReady` so the launch bootstrap doesn't persist an empty/half-restored
 * state. The saver itself (debounce/coalescing) is unit-tested in tabPersistence.
 */
export function useSessionPersistence(
  deps: TabSessionSaverDeps,
  { debounceMs = 500, periodicMs = 10_000 }: { debounceMs?: number; periodicMs?: number } = {},
): SessionPersistence {
  const saverRef = useRef<TabSessionSaver | null>(null);
  saverRef.current ??= new TabSessionSaver(deps);
  const readyRef = useRef(false);

  const requestSave = useCallback(() => {
    if (readyRef.current) saverRef.current?.requestSave();
  }, []);

  const saveNow = useCallback(async () => {
    if (readyRef.current) await saverRef.current?.saveNow();
  }, []);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const sub = tabsStore.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(requestSave, debounceMs);
    });
    const interval = setInterval(requestSave, periodicMs);
    const onBlur = () => {
      void saveNow();
    };
    window.addEventListener("blur", onBlur);
    return () => {
      sub();
      clearTimeout(timer);
      clearInterval(interval);
      window.removeEventListener("blur", onBlur);
    };
  }, [requestSave, saveNow, debounceMs, periodicMs]);

  return {
    requestSave,
    saveNow,
    remember: (records) => saverRef.current?.remember(records),
    markReady: () => {
      readyRef.current = true;
    },
  };
}
