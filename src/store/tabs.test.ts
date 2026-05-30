import { expect, test } from "bun:test";
import {
  activateRelative,
  addTab,
  closeTab,
  moveTab,
  normalizeCwd,
  replaceTabs,
  resetTabsForTest,
  setTabCwd,
  tabsStore,
  tabTitle,
} from "./tabs";

test("tabs activate newly added tabs and derive cwd titles", () => {
  resetTabsForTest();

  const first = addTab({ cwd: "/tmp/one" });
  const second = addTab({ cwd: "/tmp/two" });

  expect(tabsStore.state.activeId).toBe(second);
  expect(tabsStore.state.tabs.map((tab) => tab.id)).toEqual([first, second]);
  expect(tabTitle(tabsStore.state.tabs[0])).toBe("/tmp/one");
});

test("closing the active tab activates the neighbor", () => {
  resetTabsForTest();

  const first = addTab();
  const second = addTab();
  closeTab(second);

  expect(tabsStore.state.activeId).toBe(first);
});

test("moveTab reorders without changing active tab", () => {
  resetTabsForTest();

  const first = addTab({ customTitle: "first" });
  const second = addTab({ customTitle: "second" });
  const third = addTab({ customTitle: "third" });
  moveTab(first, 3);

  expect(tabsStore.state.activeId).toBe(third);
  expect(tabsStore.state.tabs.map((tab) => tab.id)).toEqual([second, third, first]);
});

test("activateRelative wraps around the tab list", () => {
  resetTabsForTest();

  const first = addTab();
  addTab();
  activateRelative(1);

  expect(tabsStore.state.activeId).toBe(first);
});

test("replaceTabs swaps the restored launch set instead of appending", () => {
  resetTabsForTest();

  addTab({ id: "stale", cwd: "/old" });
  replaceTabs([
    { id: "one", cwd: "/one" },
    { id: "two", cwd: "/two", active: true },
  ]);

  expect(tabsStore.state.tabs.map((tab) => tab.id)).toEqual(["one", "two"]);
  expect(tabsStore.state.activeId).toBe("two");
});

test("replaceTabs repairs duplicate restored ids", () => {
  resetTabsForTest();

  const ids = replaceTabs([
    { id: "same", cwd: "/one" },
    { id: "same", cwd: "/two" },
  ]);

  expect(ids[0]).toBe("same");
  expect(ids[1]).not.toBe("same");
  expect(new Set(tabsStore.state.tabs.map((tab) => tab.id)).size).toBe(2);
});

test("cwd is explicit metadata and blank updates do not erase it", () => {
  resetTabsForTest();

  const id = addTab();
  expect(tabsStore.state.tabs[0].cwd).toBeNull();
  expect(tabTitle(tabsStore.state.tabs[0])).toBe("shell");

  setTabCwd(id, "   ");
  expect(tabsStore.state.tabs[0].cwd).toBeNull();

  setTabCwd(id, "/tmp/project");
  expect(tabsStore.state.tabs[0].cwd).toBe("/tmp/project");

  setTabCwd(id, "");
  expect(tabsStore.state.tabs[0].cwd).toBe("/tmp/project");
});

test("normalizeCwd treats empty values as unknown", () => {
  expect(normalizeCwd(null)).toBeNull();
  expect(normalizeCwd("")).toBeNull();
  expect(normalizeCwd("  ")).toBeNull();
  expect(normalizeCwd(" /tmp/project ")).toBe("/tmp/project");
});
