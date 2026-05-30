import { useEffect, useMemo, useRef, useState } from "react";
import { type Tab, tabTitle } from "../store/tabs";

interface Props {
  tabs: Tab[];
  activeId: string | null;
  onClose: () => void;
  onSelectTab: (id: string) => void;
  onNewTab: () => void;
  onCloseTab: (id: string) => void;
  onRenameTab: (id: string) => void;
  onOpenSettings: () => void;
}

type Item =
  | { kind: "tab"; id: string; label: string; hint: string }
  | { kind: "action"; id: string; label: string; hint: string; run: () => void };

/** Subsequence fuzzy match. Returns a score (higher = better) or null if no match. */
export function fuzzyScore(query: string, target: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = target.toLowerCase();
  let qi = 0;
  let score = 0;
  let streak = 0;
  let prevIdx = -1;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) {
      streak = prevIdx === ti - 1 ? streak + 1 : 1;
      score += streak * 2 + (ti === 0 ? 5 : 0);
      prevIdx = ti;
      qi++;
    }
  }
  return qi === q.length ? score : null;
}

export function CommandPalette({
  tabs,
  activeId,
  onClose,
  onSelectTab,
  onNewTab,
  onCloseTab,
  onRenameTab,
  onOpenSettings,
}: Props) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const selectedRef = useRef<HTMLButtonElement>(null);

  const items = useMemo<Item[]>(() => {
    const tabItems: Item[] = tabs.map((t) => ({
      kind: "tab",
      id: t.id,
      label: tabTitle(t),
      hint: t.cwd ?? "",
    }));
    const actions: Item[] = [
      { kind: "action", id: "new", label: "New tab", hint: "⌘T", run: onNewTab },
      {
        kind: "action",
        id: "rename",
        label: "Rename active tab",
        hint: "F2",
        run: () => activeId && onRenameTab(activeId),
      },
      {
        kind: "action",
        id: "close",
        label: "Close active tab",
        hint: "⌘W",
        run: () => activeId && onCloseTab(activeId),
      },
      { kind: "action", id: "settings", label: "Open settings", hint: "⌘,", run: onOpenSettings },
    ];
    return [...tabItems, ...actions];
  }, [tabs, activeId, onNewTab, onRenameTab, onCloseTab, onOpenSettings]);

  const filtered = useMemo(() => {
    const scored: Array<{ item: Item; score: number }> = [];
    for (const item of items) {
      const s = fuzzyScore(query, `${item.label} ${item.hint}`);
      if (s !== null) scored.push({ item, score: s });
    }
    if (query) scored.sort((a, b) => b.score - a.score);
    return scored.map((s) => s.item);
  }, [items, query]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reset highlight to the top whenever the query changes.
  useEffect(() => {
    setSelected(0);
  }, [query]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: re-run to scroll the highlighted row into view whenever the selection index changes.
  useEffect(() => {
    selectedRef.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  function choose(item: Item | undefined) {
    if (!item) return;
    if (item.kind === "tab") onSelectTab(item.id);
    else item.run();
    onClose();
  }

  function onKey(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((i) => Math.min(i + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(filtered[selected]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      className="absolute inset-0 z-50 flex justify-center bg-black/50 pt-[12vh]"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="h-fit w-[34rem] max-w-[92vw] overflow-hidden rounded-lg border border-white/10 bg-[#16161c] shadow-2xl">
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKey}
          placeholder="Switch tab or run a command…"
          spellCheck={false}
          className="w-full border-b border-white/10 bg-transparent px-4 py-3 text-sm text-white outline-none placeholder:text-white/30"
        />
        <div className="max-h-[50vh] overflow-y-auto py-1">
          {filtered.length === 0 ? (
            <div className="px-4 py-6 text-center text-sm text-white/30">No matches</div>
          ) : (
            filtered.map((item, index) => {
              const isSel = index === selected;
              return (
                <button
                  key={item.kind + item.id}
                  ref={isSel ? selectedRef : undefined}
                  type="button"
                  className={[
                    "flex h-11 w-full cursor-pointer items-center gap-3 px-4 text-left text-sm",
                    isSel ? "bg-white/15 text-white" : "text-white/70 hover:bg-white/8",
                  ].join(" ")}
                  onMouseMove={() => setSelected(index)}
                  onClick={() => choose(item)}
                >
                  <span
                    className={[
                      "shrink-0 rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wide",
                      item.kind === "tab"
                        ? "bg-sky-500/20 text-sky-200"
                        : "bg-white/10 text-white/50",
                    ].join(" ")}
                  >
                    {item.kind === "tab" ? "tab" : "cmd"}
                  </span>
                  <span className="flex-1 truncate">{item.label}</span>
                  {item.hint && (
                    <span className="shrink-0 truncate text-xs text-white/30">{item.hint}</span>
                  )}
                </button>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
