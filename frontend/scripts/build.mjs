import { mkdir, copyFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
await import("./export-ui-sdk.mjs");
const result = spawnSync(
  process.execPath,
  ["node_modules/typescript/bin/tsc"],
  { stdio: "inherit" },
);
if (result.status !== 0) process.exit(result.status ?? 1);
await mkdir("dist", { recursive: true });
await Promise.all(
  ["index.html", "styles.css", "okami-mountains.svg", "okami-botanical.svg"].map((name) =>
    copyFile(`public/${name}`, `dist/${name}`),
  ),
);
