import { expect, test } from "bun:test";
import { fuzzyScore } from "./CommandPalette";

test("fuzzyScore matches subsequences", () => {
  expect(fuzzyScore("nt", "New tab")).toBeGreaterThan(0);
});

test("fuzzyScore rejects missing characters", () => {
  expect(fuzzyScore("xyz", "New tab")).toBeNull();
});

test("fuzzyScore rewards tighter matches", () => {
  const tight = fuzzyScore("tab", "tab");
  const loose = fuzzyScore("tab", "t a b");

  expect(tight).not.toBeNull();
  expect(loose).not.toBeNull();
  expect(tight as number).toBeGreaterThan(loose as number);
});
