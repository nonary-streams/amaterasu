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
  [
    "index.html",
    "styles.css",
    "okami-botanical.svg",
    "washi-paper.webp",
    "sun-crest.svg",
    "scroll-theme.css",
    "ink-components.css",
    "ink-summary.css",
    "console-shell.css",
    "overview-scroll.css",
    "scroll-masthead.svg",
    "scroll-foothills.svg",
    "scroll-blossoms.svg",
    "ink-swipe.svg",
    "ink-seal.svg",
    "kaushan-script.woff2",
  ].map((name) => copyFile(`public/${name}`, `dist/${name}`)),
);
