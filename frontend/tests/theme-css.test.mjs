import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../public/styles.css", import.meta.url), "utf8");

function themeTokens(pattern) {
  const block = css.match(pattern)?.[1];
  assert.ok(block, "missing theme token block");
  return Object.fromEntries([...block.matchAll(/--([a-z-]+):\s*(#[a-f0-9]{6})/gi)].map(([, name, value]) => [name, value]));
}

function linearChannel(value) {
  const channel = Number.parseInt(value, 16) / 255;
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}
function luminance(hex) {
  const channels = [1, 3, 5].map((index) => linearChannel(hex.slice(index, index + 2)));
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
}
function contrastRatio(first, second) {
  const a = luminance(first), b = luminance(second);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}
function mixSrgb(first, second, firstWeight) {
  const a = [1, 3, 5].map((index) => Number.parseInt(first.slice(index, index + 2), 16));
  const b = [1, 3, 5].map((index) => Number.parseInt(second.slice(index, index + 2), 16));
  return "#" + a.map((channel, index) => Math.round(channel * firstWeight + b[index] * (1 - firstWeight)).toString(16).padStart(2, "0")).join("");
}

const themes = [
  ["dark", /:root,\s*\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/],
  ["light", /\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/],
];
for (const [name, pattern] of themes) {
  test(name + " primary and selected states meet 4.5:1 text contrast", () => {
    const tokens = themeTokens(pattern);
    const backgrounds = [tokens.accent, tokens['accent-hover'], mixSrgb(tokens.accent, tokens.gold, 0.84)];
    for (const background of backgrounds)
      assert.ok(contrastRatio(tokens['on-accent'], background) >= 4.5, name + " primary text contrast below 4.5:1 on " + background);
    assert.ok(contrastRatio(tokens.fg, tokens['selected-bg']) >= 4.5, name + " selected radio text contrast below 4.5:1");
    assert.ok(contrastRatio(tokens['selected-accent'], tokens['selected-bg']) >= 4.5, name + " selected navigation text contrast below 4.5:1");
    assert.ok(contrastRatio(tokens.fg, tokens.panel) >= 4.5, name + " unselected radio text contrast below 4.5:1");
    assert.ok(contrastRatio(tokens['selected-accent'], tokens['selected-bg']) >= 4.5, name + " selected navigation text contrast below 4.5:1");
  });
}
test("radio appearance targets the shared radio-group primitives", () => {
  assert.match(css, /\.button\.nav-item\.selected\s*\{[^}]*color: var\(--selected-accent\);[^}]*background: var\(--selected-bg\)/s);
  assert.match(css, /\.radio-group-label\s*\{/);
  assert.match(css, /\.radio-option:has\(input\[type="radio"\]:checked\)/);
  assert.match(css, /\.radio-option:focus-within/);
  assert.doesNotMatch(css, /\.plugin-radio-(?:group|option)/);
});
