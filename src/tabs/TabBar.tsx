import { Plus, Settings, X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { IconButton } from "../components/IconButton";
import { WindowControls } from "../components/WindowControls";
import type { TabLayout } from "../lib/ipc";
import { moveTab, type Tab, tabTitle } from "../store/tabs";
import { TabContextMenu } from "./TabContextMenu";

interface Props {
  layout: TabLayout;
  paintRevision?: number;
  tabs: Tab[];
  activeId: string | null;
  editingId: string | null;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onAdd: () => void;
  onNewTabAfter: (id: string) => void;
  onStartRename: (id: string) => void;
  onCommitRename: (id: string, title: string) => void;
  onCancelRename: () => void;
  onOpenSettings: () => void;
}

const TAB_SIDEBAR_WIDTH_KEY = "phantom.tabSidebarWidth";
const TAB_SIDEBAR_MIN = 144;
const TAB_SIDEBAR_MAX = 360;
const TAB_SIDEBAR_DEFAULT = 208;

interface TitleBarChromeProps {
  paintRevision?: number;
  onOpenSettings: () => void;
}

export function TitleBarChrome({ paintRevision = 0, onOpenSettings }: TitleBarChromeProps) {
  return (
    <div
      data-tauri-drag-region
      className="app-chrome flex h-10 shrink-0 items-stretch gap-1 border-b px-1 will-change-transform"
      style={{
        transform: paintRevision % 2 === 0 ? "translateZ(0)" : "translateZ(0.001px)",
      }}
    >
      <WindowControls placement="leading" />
      <div aria-hidden className="min-w-0 flex-1" data-tauri-drag-region />
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
  );
}

export function TabBar({
  layout,
  paintRevision = 0,
  tabs,
  activeId,
  editingId,
  onActivate,
  onClose,
  onAdd,
  onNewTabAfter,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onOpenSettings,
}: Props) {
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const activeIndex = tabs.findIndex((tab) => tab.id === activeId);
  // Drag-to-reorder state. `dragId` is the tab being dragged; `dropGap` is the
  // gap (0..tabs.length) where it would be inserted, used to draw the marker.
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropGap, setDropGap] = useState<number | null>(null);

  // The tab strip hides its scrollbar; instead we fade the strip's edges to
  // signal that tabs continue off-screen. `overflow` tracks which side(s) have
  // hidden tabs so each fade only shows when it's actually scrollable that way.
  const scrollRef = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState({ before: false, after: false });
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const saved = Number(localStorage.getItem(TAB_SIDEBAR_WIDTH_KEY));
    return saved >= TAB_SIDEBAR_MIN && saved <= TAB_SIDEBAR_MAX ? saved : TAB_SIDEBAR_DEFAULT;
  });

  const updateOverflow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (layout === "vertical") {
      const { scrollTop, scrollHeight, clientHeight } = el;
      setOverflow({
        before: scrollTop > 1,
        after: scrollTop + clientHeight < scrollHeight - 1,
      });
      return;
    }
    const { scrollLeft, scrollWidth, clientWidth } = el;
    setOverflow({
      before: scrollLeft > 1,
      after: scrollLeft + clientWidth < scrollWidth - 1,
    });
  }, [layout]);

  const startSidebarResize = useCallback(
    (e: React.MouseEvent) => {
      if (layout !== "vertical") return;
      e.preventDefault();

      const onMove = (ev: MouseEvent) => {
        const next = Math.max(TAB_SIDEBAR_MIN, Math.min(TAB_SIDEBAR_MAX, ev.clientX));
        setSidebarWidth(next);
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };

      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [layout],
  );

  useEffect(() => {
    localStorage.setItem(TAB_SIDEBAR_WIDTH_KEY, String(sidebarWidth));
  }, [sidebarWidth]);

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

  const marker = (gap: number, edge: "before" | "after" = "before") => {
    if (dragId == null || dropGap !== gap) return null;
    return (
      <div
        aria-hidden
        className={[
          "app-drop-marker pointer-events-none absolute z-10 rounded-full",
          layout === "vertical"
            ? `${edge === "after" ? "right-2 bottom-0 left-2" : "top-0 right-2 left-2"} h-0.5`
            : `${edge === "after" ? "top-1.5 right-0 bottom-1.5" : "top-1.5 bottom-1.5 left-0"} w-0.5`,
        ].join(" ")}
      />
    );
  };

  const menuNode = menu && (
    <TabContextMenu
      x={menu.x}
      y={menu.y}
      onNewTab={() => {
        onNewTabAfter(menu.id);
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
  );

  if (layout === "vertical") {
    return (
      <>
        <aside className="app-sidebar flex shrink-0 flex-col" style={{ width: sidebarWidth }}>
          <div className="flex h-10 shrink-0 items-center gap-2 border-white/10 border-b px-2">
            <span className="min-w-0 flex-1 truncate px-1 font-semibold text-white/40 text-xs uppercase tracking-wide">
              Tabs
            </span>
            <IconButton
              icon={Plus}
              label="New tab"
              title="New tab (⌘T)"
              size={17}
              className="h-7 w-7 shrink-0"
              onClick={onAdd}
            />
          </div>
          <div className="relative min-h-0 flex-1">
            <div
              ref={scrollRef}
              role="tablist"
              aria-label="Terminal tabs"
              aria-orientation="vertical"
              onScroll={updateOverflow}
              className="tab-scrollbar flex h-full flex-col overflow-y-auto py-1"
              onDragOver={(e) => {
                // Allow dropping past the last tab (in the trailing empty space).
                if (dragId == null) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
              }}
              onDrop={onDrop}
            >
              {tabs.map((tab, i) => (
                <div
                  // biome-ignore lint/suspicious/noArrayIndexKey: duplicated restored ids are repaired by addTab, but index keeps this render stable during repair.
                  key={`${tab.id}-${i}`}
                  data-tab-id={tab.id}
                  className="no-drag relative flex flex-col"
                >
                  {marker(i)}
                  {i > 0 && (
                    <div
                      aria-hidden
                      className="pointer-events-none absolute top-0 right-2 left-2 z-10 h-px bg-white/14"
                    />
                  )}
                  <TabItem
                    layout={layout}
                    tab={tab}
                    index={i}
                    active={i === activeIndex}
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
              <div className="relative h-0 shrink-0">{marker(tabs.length, "after")}</div>
            </div>
            <div
              aria-hidden
              className={[
                "chrome-scroll-fade chrome-scroll-fade--top pointer-events-none absolute top-0 right-0 left-0 h-8 transition-opacity",
                overflow.before ? "opacity-100" : "opacity-0",
              ].join(" ")}
            />
            <div
              aria-hidden
              className={[
                "chrome-scroll-fade chrome-scroll-fade--bottom pointer-events-none absolute right-0 bottom-0 left-0 h-8 transition-opacity",
                overflow.after ? "opacity-100" : "opacity-0",
              ].join(" ")}
            />
          </div>
        </aside>
        {/* biome-ignore lint/a11y/useSemanticElements: a focusable window splitter is the standard ARIA pattern, not an <hr>. */}
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize tab sidebar"
          aria-valuenow={Math.round(sidebarWidth)}
          aria-valuemin={TAB_SIDEBAR_MIN}
          aria-valuemax={TAB_SIDEBAR_MAX}
          tabIndex={0}
          onMouseDown={startSidebarResize}
          onKeyDown={(e) => {
            if (e.key === "ArrowLeft") {
              e.preventDefault();
              setSidebarWidth((w) => Math.max(TAB_SIDEBAR_MIN, w - 16));
            } else if (e.key === "ArrowRight") {
              e.preventDefault();
              setSidebarWidth((w) => Math.min(TAB_SIDEBAR_MAX, w + 16));
            }
          }}
          className="no-drag group relative w-1 shrink-0 cursor-col-resize outline-none"
        >
          <div className="absolute inset-y-0 left-0 w-px bg-white/10 group-hover:bg-sky-400/60 group-focus:bg-sky-400/60" />
        </div>
        {menuNode}
      </>
    );
  }

  return (
    <>
      <div
        data-tauri-drag-region
        className="app-chrome flex h-10 shrink-0 items-stretch gap-1 border-b px-1 will-change-transform"
        style={{
          transform: paintRevision % 2 === 0 ? "translateZ(0)" : "translateZ(0.001px)",
        }}
      >
        <WindowControls placement="leading" />
        {/* Sizes to the fixed-width tabs, but shrinks (min-w-0) to scroll when they
            overflow. Because it isn't flex-1, the New Tab button after it sits
            right next to the tabs when they fit and pins to the right edge of
            the strip once they overflow. */}
        <div className="relative flex min-w-0 items-stretch">
          <div
            ref={scrollRef}
            role="tablist"
            aria-label="Terminal tabs"
            onScroll={updateOverflow}
            className="tab-scrollbar flex items-stretch overflow-x-auto px-1"
            onDragOver={(e) => {
              // Allow dropping past the last tab (in the trailing empty space).
              if (dragId == null) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
            }}
            onDrop={onDrop}
          >
            {tabs.map((tab, i) => (
              <div
                // biome-ignore lint/suspicious/noArrayIndexKey: duplicated restored ids are repaired by addTab, but index keeps this render stable during repair.
                key={`${tab.id}-${i}`}
                data-tab-id={tab.id}
                className="no-drag relative flex shrink-0 items-stretch"
              >
                {marker(i)}
                {i > 0 && (
                  <div
                    aria-hidden
                    className="pointer-events-none absolute top-2 bottom-2 left-0 z-10 w-px bg-white/18"
                  />
                )}
                <TabItem
                  layout={layout}
                  tab={tab}
                  index={i}
                  active={i === activeIndex}
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
            <div className="relative w-0 shrink-0">{marker(tabs.length, "after")}</div>
          </div>
          {/* Edge fades: shown only when tabs are scrolled off that side. They
              take no layout space (absolute) and let clicks/drags pass through. */}
          <div
            aria-hidden
            className={[
              "chrome-scroll-fade chrome-scroll-fade--left pointer-events-none absolute inset-y-0 left-0 w-8 transition-opacity",
              overflow.before ? "opacity-100" : "opacity-0",
            ].join(" ")}
          />
          <div
            aria-hidden
            className={[
              "chrome-scroll-fade chrome-scroll-fade--right pointer-events-none absolute inset-y-0 right-0 w-8 transition-opacity",
              overflow.after ? "opacity-100" : "opacity-0",
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
      {menuNode}
    </>
  );
}

interface ItemProps {
  layout: TabLayout;
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
  layout,
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
  const title = tabTitle(tab);
  const preserveTitleEnd = tab.kind === "terminal" && !tab.customTitle?.trim() && Boolean(tab.cwd);

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
        // Leading half → insert before this tab; trailing half → after it.
        const r = e.currentTarget.getBoundingClientRect();
        const after =
          layout === "vertical"
            ? e.clientY > r.top + r.height / 2
            : e.clientX > r.left + r.width / 2;
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
        "no-drag group relative flex cursor-pointer items-center gap-1 bg-transparent text-sm transition-colors",
        layout === "vertical" ? "min-h-10 w-full px-2.5" : "h-full w-48 shrink-0 pr-2 pl-3",
        active ? "text-white" : "text-white/55 hover:bg-white/[0.07] hover:text-white/85",
        dragging ? "opacity-40" : "",
      ].join(" ")}
      title={tab.cwd ?? title}
    >
      {active && (
        <span
          aria-hidden
          className={
            layout === "vertical"
              ? "app-active-tab-indicator absolute top-1.5 right-0 bottom-1.5 w-0.5 rounded-l-full"
              : "app-active-tab-indicator absolute right-2 bottom-0 left-2 h-0.5 rounded-full"
          }
        />
      )}
      {editing ? (
        <input
          ref={inputRef}
          defaultValue={tab.customTitle ?? ""}
          placeholder={title}
          className="min-w-0 flex-1 bg-transparent text-white outline-none placeholder:text-white/40"
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
        <TabTitle title={title} preserveEnd={preserveTitleEnd} />
      )}
      <button
        type="button"
        aria-label="Close tab"
        className="flex h-5 w-5 shrink-0 items-center justify-center rounded text-white/40 opacity-0 hover:bg-white/20 hover:text-white group-hover:opacity-100 focus-visible:opacity-100"
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

function TabTitle({ title, preserveEnd }: { title: string; preserveEnd: boolean }) {
  const ref = useRef<HTMLSpanElement>(null);
  const [displayTitle, setDisplayTitle] = useState(title);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !preserveEnd) {
      setDisplayTitle(title);
      return;
    }

    const update = () => {
      setDisplayTitle(leftElideToFit(title, el));
    };

    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    document.fonts?.ready.then(update).catch(() => undefined);
    return () => ro.disconnect();
  }, [title, preserveEnd]);

  return (
    <span ref={ref} className="min-w-0 flex-1 overflow-hidden whitespace-nowrap pr-1">
      {displayTitle}
    </span>
  );
}

let measureCanvas: HTMLCanvasElement | null = null;

function leftElideToFit(text: string, el: HTMLElement): string {
  const width = availableTextWidth(el);
  if (width <= 0 || measureText(text, el) <= width) return text;

  const chars = Array.from(text);
  let lo = 0;
  let hi = chars.length;
  let best = "…";

  while (lo <= hi) {
    const mid = Math.floor((lo + hi) / 2);
    const candidate = `…${chars.slice(chars.length - mid).join("")}`;
    if (measureText(candidate, el) <= width) {
      best = candidate;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }

  return best;
}

function availableTextWidth(el: HTMLElement): number {
  const style = getComputedStyle(el);
  const padding =
    (Number.parseFloat(style.paddingLeft) || 0) + (Number.parseFloat(style.paddingRight) || 0);
  return Math.max(0, el.clientWidth - padding);
}

function measureText(text: string, el: HTMLElement): number {
  measureCanvas ??= document.createElement("canvas");
  const context = measureCanvas.getContext("2d");
  if (!context) return Number.POSITIVE_INFINITY;
  context.font = getComputedStyle(el).font;
  return context.measureText(text).width;
}
