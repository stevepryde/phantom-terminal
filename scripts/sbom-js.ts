#!/usr/bin/env bun
import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";

interface PackageJson {
  name: string;
  version?: string;
}

interface Component {
  type: "library";
  name: string;
  version: string;
  purl: string;
  scope?: "required" | "optional";
}

const root = resolve(import.meta.dir, "..");
const packageJson = (await Bun.file(resolve(root, "package.json")).json()) as PackageJson;
const lockText = await Bun.file(resolve(root, "bun.lock")).text();
const output = resolve(root, process.argv[2] ?? "sbom-js.json");

const components = parseBunLock(lockText);
const now = new Date().toISOString();
const bom = {
  bomFormat: "CycloneDX",
  specVersion: "1.6",
  serialNumber: `urn:uuid:${crypto.randomUUID()}`,
  version: 1,
  metadata: {
    timestamp: now,
    tools: [
      {
        type: "application",
        vendor: "Phantom Terminal",
        name: "scripts/sbom-js.ts",
        version: packageJson.version ?? "0.0.0",
      },
    ],
    component: {
      type: "application",
      name: packageJson.name,
      version: packageJson.version ?? "0.0.0",
      purl: `pkg:npm/${packageJson.name}@${packageJson.version ?? "0.0.0"}`,
    },
  },
  components,
};

await mkdir(dirname(output), { recursive: true });
await Bun.write(output, `${JSON.stringify(bom, null, 2)}\n`);
console.log(`Wrote ${components.length} JS components to ${output}`);

function parseBunLock(text: string): Component[] {
  const components = new Map<string, Component>();
  for (const line of text.split(/\r?\n/)) {
    const match = line.match(/^\s+"[^"]+": \["([^"]+@[^"]+)"/);
    if (!match) continue;
    const spec = match[1];
    const at = spec.lastIndexOf("@");
    if (at <= 0) continue;
    const name = spec.slice(0, at);
    const version = spec.slice(at + 1);
    if (!name || !version) continue;
    const key = `${name}@${version}`;
    components.set(key, {
      type: "library",
      name,
      version,
      purl: npmPurl(name, version),
    });
  }
  return [...components.values()].sort((a, b) =>
    `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`),
  );
}

function npmPurl(name: string, version: string): string {
  return `pkg:npm/${encodeURIComponent(name).replace("%2F", "/")}@${encodeURIComponent(version)}`;
}
