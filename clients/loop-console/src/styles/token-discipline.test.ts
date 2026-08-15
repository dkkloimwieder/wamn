// @vitest-environment node
// A filesystem lint, not a component test: under jsdom `import.meta.url` is an
// http: URL and cannot be resolved to a path.
import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const packageRoot = fileURLToPath(new URL("../..", import.meta.url));
const tokensFile = join(packageRoot, "src", "styles", "tokens.css");

/** Build output and installed packages are not ours to police. */
const skipped = new Set(["node_modules", "dist"]);

/**
 * Needles are spelled with a throwaway character class so this scanner does
 * not trip over its own source when it walks the tree.
 */
const banned: ReadonlyArray<{ what: string; pattern: RegExp }> = [
  { what: "hex color", pattern: /#[0-9a-fA-F]{3,8}\b/ },
  {
    what: "color function",
    pattern: /\b(?:rgba?|hsla?|oklch|oklab|lab|lch|color-mix|color)\s*\(/,
  },
  {
    // any color-bearing declaration whose value is not a token — this is what
    // catches the CSS keyword names the hex and function patterns miss
    what: "non-token color",
    pattern:
      /\b(?:color|background|background-color|border(?:-\w+)?-color|outline-color|fill|stroke)\s*:\s*(?!var\(|inherit|currentColor|transparent|none)[a-z]/,
  },
  {
    // the shorthands that carry a color inside a longer value; the whole
    // declaration has to reach for a token somewhere
    what: "non-token border",
    pattern:
      /\b(?:border(?:-top|-right|-bottom|-left)?|outline)\s*:(?![^;]*(?:var\(|none|inherit))[^;]*[a-z]/,
  },
  {
    // the css declaration, plus both JSX inline-style spellings of it
    what: "font-family declaration",
    pattern: /font-[f]amily["']?\s*:|font[F]amily\s*:/,
  },
  { what: "font shorthand", pattern: /\bfont["']?\s*:/ },
  { what: "font-face rule", pattern: /@font-[f]ace/ },
  { what: "stylesheet import", pattern: /@import\s+url/ },
  {
    // the shape a webfont would arrive in: index.html has no stylesheet of
    // its own, the app imports its two css files through main.tsx
    what: "external stylesheet link",
    pattern: /<link\b[^>]*\brel=["']?(?:stylesheet|preconnect)/,
  },
];

function packageFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    if (skipped.has(entry.name)) {
      return [];
    }
    const path = join(dir, entry.name);
    return entry.isDirectory() ? packageFiles(path) : [path];
  });
}

describe("token discipline", () => {
  it("finds no color or font literal outside tokens.css", () => {
    const offences: string[] = [];
    for (const path of packageFiles(packageRoot)) {
      if (path === tokensFile) {
        continue;
      }
      const content = readFileSync(path, "utf8");
      for (const { what, pattern } of banned) {
        const hit = pattern.exec(content);
        if (hit !== null) {
          offences.push(`${relative(packageRoot, path)}: ${what} ${JSON.stringify(hit[0])}`);
        }
      }
    }
    expect(offences).toEqual([]);
  });

  it("declares §1's palette, status colors, and both font stacks", () => {
    const tokens = readFileSync(tokensFile, "utf8");
    // hex digits are kept separate from their "#" so this file passes its own scan
    const colors = {
      "--ink": "101216",
      "--panel": "181B21",
      "--line": "2A2F38",
      "--text": "C9CED6",
      "--dim": "7A8290",
      "--signal": "E8C15A",
      "--status-ok": "4FA870",
      "--status-fail": "C4554D",
      "--status-warn": "C8933B",
      "--status-uncertain": "9A6BD0",
    };
    for (const [token, value] of Object.entries(colors)) {
      expect(tokens).toContain(`${token}: #${value};`);
    }
    expect(tokens).toContain("--status-neutral: var(--dim);");
    expect(tokens).toContain(
      "--font-data: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;",
    );
    expect(tokens).toContain("--font-frame: system-ui, sans-serif;");
  });

  it("drops motion under prefers-reduced-motion", () => {
    const tokens = readFileSync(tokensFile, "utf8");
    expect(tokens).toContain("@media (prefers-reduced-motion: reduce)");
    expect(tokens).toContain("transition: none !important;");
    expect(tokens).toContain("animation: none !important;");
  });
});
