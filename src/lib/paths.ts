// Home directory, fetched once from the backend at launch. Used to render
// `~`-relative tab names. Process-constant, so a module-level holder is fine.
let homeDir = "";

export function setHomeDir(dir: string | null | undefined): void {
  homeDir = dir ?? "";
}

/**
 * Turn an absolute working directory into a compact tab label:
 *   - collapse the home dir to `~` (e.g. `/Users/me/projects` → `~/projects`)
 *   - if still longer than `maxLen`, keep the trailing path segments that fit,
 *     prefixed with an ellipsis (e.g. `…projects/some-name`)
 *
 * Returns "" for an empty cwd so callers can fall back to a default label.
 */
export function formatCwdName(cwd: string, maxLen = 22): string {
  if (!cwd) return "";

  let path = cwd;
  if (homeDir && (path === homeDir || path.startsWith(`${homeDir}/`))) {
    path = `~${path.slice(homeDir.length)}`;
  }
  if (path.length <= maxLen) return path;

  const segments = path.split("/").filter(Boolean);
  // Grow the tail one segment at a time until adding another would overflow
  // (+1 accounts for the leading ellipsis character).
  let tail = segments[segments.length - 1] ?? path;
  for (let i = segments.length - 2; i >= 0; i--) {
    const candidate = `${segments[i]}/${tail}`;
    if (candidate.length + 1 > maxLen) break;
    tail = candidate;
  }
  // Even a single segment can exceed maxLen — hard-truncate it from the left.
  if (tail.length + 1 > maxLen) {
    tail = tail.slice(tail.length - (maxLen - 1));
  }
  return `…${tail}`;
}
