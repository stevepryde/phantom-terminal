import { Plus, Settings, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { IconButton } from "../components/IconButton";
import { WindowControls } from "../components/WindowControls";
import { moveTab, type Tab, tabTitle } from "../store/tabs";
import { TabContextMenu } from "./TabContextMenu";

interface Props {
  tabs: Tab[];
  activeId: string | null;
  editingId: string | null;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onAdd: () => void;
  onNewTabRight: (id: string) => void;
  onStartRename: (id: string) => void;
  onCommitRename: (id: string, title: string) => void;
  onCancelRename: () => void;
  onOpenSettings: () => void;
}

export function TabBar({
  tabs,
  activeId,
  editingId,
  onActivate,
  onClose,
  onAdd,
  onNewTabRight,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onOpenSettings,
}: Props) {
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  // Drag-to-reorder state. `dragId` is the tab being dragged; `dropGap` is the
  // gap (0..tabs.length) where it would be inserted, used to draw the marker.
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropGap, setDropGap] = useState<number | null>(null);

  // The tab strip hides its scrollbar; instead we fade the strip's edges to
  // signal that tabs continue off-screen. `overflow` tracks which side(s) have
  // hidden tabs so each fade only shows when it's actually scrollable that way.
  const scrollRef = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState({ left: false, right: false });

  const updateOverflow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const { scrollLeft, scrollWidth, clientWidth } = el;
    setOverflow({
      left: scrollLeft > 1,
      right: scrollLeft + clientWidth < scrollWidth - 1,
    });
  }, []);

  // Recompute on mount and whenever the strip resizes (window resize, sidebar
  // changes, etc.). A ResizeObserver catches size changes the scroll handler
  // can't.
  useEffect(() => {
    updateOverflow();
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(updateOverflow);
    ro.observe(el);
    return () => ro.disconnect();
  }, [updateOverflow]);

  // Adding/removing tabs changes the scrollable content width without resizing
  // the strip, so re-check when the tab count changes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: re-run on tab count change to refresh the fades.
  useEffect(updateOverflow, [tabs.length, updateOverflow]);

  // Scroll the active tab into view whenever it changes, so newly-opened tabs
  // (terminal or settings) and far-off tabs selected via shortcut/palette are
  // never left off-screen. `nearest` avoids jumping when it's already visible.
  useEffect(() => {
    if (!activeId) return;
    const el = scrollRef.current?.querySelector(`[data-tab-id="${CSS.escape(activeId)}"]`);
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeId]);

  const endDrag = () => {
    setDragId(null);
    setDropGap(null);
  };

  const onDrop = () => {
    if (dragId != null && dropGap != null) moveTab(dragId, dropGap);
    endDrag();
  };

  const marker = (gap: number) => (
    <div
      // Insertion marker shown at the active drop gap.
      className={[
        "my-1.5 w-0.5 shrink-0 self-stretch rounded-full",
        dragId != null && dropGap === gap ? "bg-sky-400" : "bg-transparent",
      ].join(" ")}
    />
  );

  return (
    <>
      <div
        data-tauri-drag-region
        className="flex h-10 shrink-0 items-stretch gap-1 border-white/10 border-b bg-[#111116] px-1"
      >
        <WindowControls placement="leading" />
        {/* Sizes to the tabs' width, but shrinks (min-w-0) to scroll when they
            overflow. Because it isn't flex-1, the New Tab button after it sits
            right next to the tabs when they fit and pins to the right edge of
            the strip once they overflow. */}
        <div className="relative flex min-w-0 items-stretch">
          <div
            ref={scrollRef}
            role="tablist"
            aria-label="Terminal tabs"
            onScroll={updateOverflow}
            className="tab-scrollbar flex items-stretch gap-1 overflow-x-auto px-1"
            onDragOver={(e) => {
              // Allow dropping past the last tab (in the trailing empty space).
              if (dragId == null) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
            }}
            onDrop={onDrop}
          >
            {tabs.map((tab, i) => (
              <div key={tab.id} data-tab-id={tab.id} className="no-drag flex items-stretch">
                {marker(i)}
                <TabItem
                  tab={tab}
                  index={i}
                  active={tab.id === activeId}
                  editing={tab.id === editingId}
                  dragging={tab.id === dragId}
                  onActivate={onActivate}
                  onClose={onClose}
                  onCommitRename={onCommitRename}
                  onCancelRename={onCancelRename}
                  onContextMenu={(id, x, y) => {
                    onActivate(id);
                    setMenu({ id, x, y });
                  }}
                  onDragStart={(id) => setDragId(id)}
                  onDragEnd={endDrag}
                  onDragOverTab={(gap) => setDropGap(gap)}
                  onDropTab={onDrop}
                />
              </div>
            ))}
            {marker(tabs.length)}
          </div>
          {/* Edge fades: shown only when tabs are scrolled off that side. They
              take no layout space (absolute) and let clicks/drags pass through. */}
          <div
            aria-hidden
            className={[
              "pointer-events-none absolute inset-y-0 left-0 w-8 bg-gradient-to-r from-[#111116] to-transparent transition-opacity",
              overflow.left ? "opacity-100" : "opacity-0",
            ].join(" ")}
          />
          <div
            aria-hidden
            className={[
              "pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l from-[#111116] to-transparent transition-opacity",
              overflow.right ? "opacity-100" : "opacity-0",
            ].join(" ")}
          />
        </div>
        <IconButton
          icon={Plus}
          label="New tab"
          title="New tab (⌘T)"
          size={18}
          className="my-1.5 w-8 shrink-0"
          onClick={onAdd}
        />
        <div className="ml-auto flex items-stretch gap-1">
          <IconButton
            icon={Settings}
            label="Settings"
            title="Settings (⌘,)"
            className="my-1.5 w-8"
            onClick={onOpenSettings}
          />
          <WindowControls placement="trailing" />
          <div aria-hidden className="h-full w-3 shrink-0" data-tauri-drag-region />
        </div>
      </div>
      {menu && (
        <TabContextMenu
          x={menu.x}
          y={menu.y}
          onNewTab={() => {
            onNewTabRight(menu.id);
            setMenu(null);
          }}
          onRename={() => {
            onStartRename(menu.id);
            setMenu(null);
          }}
          onCloseTab={() => {
            onClose(menu.id);
            setMenu(null);
          }}
          onDismiss={() => setMenu(null)}
        />
      )}
    </>
  );
}

interface ItemProps {
  tab: Tab;
  index: number;
  active: boolean;
  editing: boolean;
  dragging: boolean;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onCommitRename: (id: string, title: string) => void;
  onCancelRename: () => void;
  onContextMenu: (id: string, x: number, y: number) => void;
  onDragStart: (id: string) => void;
  onDragEnd: () => void;
  onDragOverTab: (gap: number) => void;
  onDropTab: () => void;
}

function TabItem({
  tab,
  index,
  active,
  editing,
  dragging,
  onActivate,
  onClose,
  onCommitRename,
  onCancelRename,
  onContextMenu,
  onDragStart,
  onDragEnd,
  onDragOverTab,
  onDropTab,
}: ItemProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  return (
    <div
      // Whole-tab dragging reorders tabs. Disabled while renaming so the input
      // stays text-selectable.
      draggable={!editing}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", tab.id);
        onDragStart(tab.id);
      }}
      onDragEnd={onDragEnd}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        // Left half → insert before this tab; right half → after it.
        const r = e.currentTarget.getBoundingClientRect();
        const after = e.clientX > r.left + r.width / 2;
        onDragOverTab(after ? index + 1 : index);
      }}
      onDrop={(e) => {
        e.preventDefault();
        onDropTab();
      }}
      // A single click ALWAYS just activates — never renames. This is the core
      // fix for Warp's hostile double-click-to-rename behaviour. Renaming is an
      // explicit action from the right-click menu (or F2).
      onClick={() => onActivate(tab.id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onActivate(tab.id);
        }
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu(tab.id, e.clientX, e.clientY);
      }}
      role="tab"
      aria-selected={active}
      tabIndex={0}
      className={[
        "no-drag group my-1.5 flex min-w-[7rem] max-w-[14rem] cursor-pointer items-center gap-2 rounded px-3 text-sm",
        active ? "bg-white/15 text-white" : "text-white/55 hover:bg-white/8 hover:text-white/85",
        dragging ? "opacity-40" : "",
      ].join(" ")}
      title={tab.cwd || tabTitle(tab)}
    >
      {editing ? (
        <input
          ref={inputRef}
          defaultValue={tab.customTitle ?? ""}
          placeholder={tabTitle(tab)}
          className="w-full bg-transparent text-white outline-none placeholder:text-white/40"
          onClick={(e) => e.stopPropagation()}
          onBlur={(e) => onCommitRename(tab.id, e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              onCommitRename(tab.id, e.currentTarget.value);
            } else if (e.key === "Escape") {
              e.preventDefault();
              onCancelRename();
            }
          }}
        />
      ) : (
        <span className="flex-1 truncate">{tabTitle(tab)}</span>
      )}
      <button
        type="button"
        aria-label="Close tab"
        className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-white/40 opacity-0 hover:bg-white/20 hover:text-white group-hover:opacity-100"
        title="Close tab (⌘W)"
        onClick={(e) => {
          e.stopPropagation();
          onClose(tab.id);
        }}
      >
        <X size={12} />
      </button>
    </div>
  );
}
