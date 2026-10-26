// Generated from src/main.rs by `mise run docs:examples` (tools/docs-examples): the unit-test
// fixtures, and terminal sessions captured from the real binary. Every example is derived from them.
import data from "../data/examples.json";
import terminal from "../data/terminal.json";

type Pair = [string, string];

interface Fixture {
  strings: Record<string, string>;
  expected: string[];
  entries: Pair[];
  latest: Pair[];
  files: string[];
  paths: string[];
}

const fixtures = data as unknown as Record<string, Fixture>;

export interface Session {
  cwd: string;
  command: string;
  output: { text: string; styles: string[] }[];
}

/** `typst-updater --help`, and a confirmed run over a project built from the test fixtures. */
export const terminals: { help: Session; run: Session } = terminal;

/** The fixture for a test, failing the build if the test has been renamed or removed. */
export function fixture(test: string): Fixture {
  const found = fixtures[test];
  if (!found) {
    throw new Error(`No test named ${test} in src/main.rs; update docs/src/pages/index.astro to match the tests`);
  }
  return found;
}

/** A `let` binding from a test, without the trailing newline. */
export function source(test: string, binding = "src"): string {
  const value = fixture(test).strings[binding];
  if (value === undefined) {
    throw new Error(`Test ${test} has no string binding named ${binding}`);
  }
  return value.trimEnd();
}

export interface Import {
  line: number;
  name: string;
  version: string;
}

/** Mirrors IMPORT_RE in src/main.rs. */
const IMPORT_RE = /\bimport\s+"@preview\/([A-Za-z0-9_-]+):([0-9]+\.[0-9]+\.[0-9]+)"/g;

export function importsIn(src: string): Import[] {
  return [...src.matchAll(IMPORT_RE)].map((m) => ({
    line: src.slice(0, m.index).split("\n").length,
    name: m[1],
    version: m[2],
  }));
}

/** Plain major.minor.patch versions as a number triple, or undefined when unparseable. */
function parse(version: string): number[] | undefined {
  return /^\d+\.\d+\.\d+$/.test(version) ? version.split(".").map(Number) : undefined;
}

export function compare(a: string, b: string): number {
  const [x, y] = [parse(a)!, parse(b)!];
  return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
}

export function isVersion(version: string): boolean {
  return parse(version) !== undefined;
}

/** The highest valid version of each package, as latest_versions computes it. */
export function latestVersions(entries: Pair[]): Map<string, string> {
  const latest = new Map<string, string>();
  for (const [name, version] of entries) {
    if (!isVersion(version)) continue;
    const current = latest.get(name);
    if (current === undefined || compare(version, current) > 0) latest.set(name, version);
  }
  return latest;
}

/** Renders package index entries the way they appear in packages.typst.org/preview/index.json. */
export function indexJson(entries: Pair[]): string {
  const width = Math.max(...entries.map(([name]) => name.length + 3));
  const rows = entries.map(
    ([name, version]) => `  { "name": ${`"${name}",`.padEnd(width)} "version": "${version}" }`,
  );
  return `[\n${rows.join(",\n")}\n]`;
}

/**
 * Draws the files a test creates as a directory tree, noting which ones the walk found.
 * Entries are listed in name order, as WalkDir::sort_by_file_name visits them.
 */
export function fileTree(root: string, files: string[], found: string[]): string {
  type Dir = Map<string, Dir | null>;
  const tree: Dir = new Map();
  for (const file of files) {
    const parts = file.split("/");
    let dir = tree;
    for (const part of parts.slice(0, -1)) {
      if (!dir.has(part)) dir.set(part, new Map());
      dir = dir.get(part)!;
    }
    dir.set(parts.at(-1)!, null);
  }

  const note = (path: string) => {
    if (found.includes(path)) return "scanned";
    if (path.split("/").some((p) => p.startsWith("."))) return "skipped: hidden directory";
    if (!path.endsWith(".typ")) return "skipped: not a .typ file";
    return "skipped";
  };

  const rows: [string, string][] = [];
  const walk = (dir: Dir, prefix: string, path: string) => {
    const names = [...dir.keys()].sort();
    names.forEach((name, i) => {
      const last = i === names.length - 1;
      const child = dir.get(name)!;
      const label = `${prefix}${last ? "└── " : "├── "}${name}${child ? "/" : ""}`;
      rows.push([label, child ? "" : note(path + name)]);
      if (child) walk(child, prefix + (last ? "    " : "│   "), `${path}${name}/`);
    });
  };
  walk(tree, "", "");

  const width = Math.max(...rows.map(([label]) => label.length)) + 3;
  return [`${root}/`, ...rows.map(([label, n]) => (n ? label.padEnd(width) + n : label))].join("\n");
}
