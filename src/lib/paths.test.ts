import { expect, test } from "bun:test";
import { formatCwdName, setHomeDir } from "./paths";

test("formatCwdName collapses the home directory", () => {
  setHomeDir("/Users/steve");
  expect(formatCwdName("/Users/steve/project")).toBe("~/project");
});

test("formatCwdName keeps the tail of long paths", () => {
  setHomeDir("");
  expect(formatCwdName("/very/long/path/to/phantom-terminal", 18)).toBe("…phantom-terminal");
});

test("formatCwdName returns empty for empty cwd", () => {
  expect(formatCwdName("")).toBe("");
});
