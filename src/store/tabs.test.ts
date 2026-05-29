import { expect, test } from "bun:test";
import {
  activateRelative,
  addTab,
  closeTab,
  moveTab,
  resetTabsForTest,
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
