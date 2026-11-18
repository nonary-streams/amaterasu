// Mount plugins in the same source and stylesheet order as the console, without starting its app.
import { readFileSync, readdirSync } from "node:fs";
import ts from "typescript";

const root = new URL("../", import.meta.url);
const project = JSON.parse(readFileSync(new URL("tsconfig.json", root), "utf8"));
export const consoleSource = ts.transpile(project.files
  .filter((file) => file.startsWith("src/") && file !== "src/app.ts")
  .map((file) => readFileSync(new URL(file, root), "utf8")).join("\n"),
{ target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None });

const publicRoot = new URL("public/", root);
const index = readFileSync(new URL("index.html", publicRoot), "utf8");
export const consoleStyles = index.match(/<link rel="stylesheet" href="[^"]+"\s*\/>/g).join("\n");
const contentTypes = { css: "text/css", svg: "image/svg+xml", woff2: "font/woff2", webp: "image/webp" };
const assets = new Map(readdirSync(publicRoot)
  .map((name) => [name, contentTypes[name.split(".").at(-1)]])
  .filter(([, type]) => type));

export function serveConsoleAsset(request, response) {
  const name = request.url.slice(1);
  if (!assets.has(name)) return false;
  response.setHeader("Content-Type", assets.get(name));
  response.end(readFileSync(new URL(name, publicRoot)));
  return true;
}
