#!/usr/bin/env node
/**
 * CLI for compiling TypeScript OaaS services into WASM Components.
 *
 * Convention-based: reads `src/index.ts` from CWD, outputs `dist/<name>.wasm`.
 * The project name is derived from package.json `name` field.
 *
 * Usage:
 *   oprc-compile                        # convention-based
 *   oprc-compile --source src/foo.ts    # custom source
 *   oprc-compile --output out/my.wasm   # custom output
 *   oprc-compile --disable http,stdio   # disable WASI features
 *   oprc-compile --timeout 60000        # custom timeout (ms)
 */

import { parseArgs } from "node:util";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { compileTypeScript } from "./compiler.js";

type DisableFeature = 'stdio' | 'random' | 'clocks' | 'http' | 'fetch-event';
const VALID_DISABLE_FEATURES = new Set<string>(['stdio', 'random', 'clocks', 'http', 'fetch-event']);

const WASM_MAGIC = new Uint8Array([0x00, 0x61, 0x73, 0x6d]);

// Resolve WIT and SDK paths relative to this module (the compiler package)
const __dirname = path.dirname(fileURLToPath(import.meta.url));

function resolveDefaultPath(relativePath: string): string {
  return path.resolve(__dirname, relativePath);
}

interface CliOptions {
  source: string;
  output: string;
  timeout: number;
  disableFeatures: DisableFeature[];
}

async function readProjectName(cwd: string): Promise<string> {
  const pkgPath = path.join(cwd, "package.json");
  try {
    const raw = await fs.readFile(pkgPath, "utf-8");
    const pkg = JSON.parse(raw) as { name?: string };
    if (pkg.name) {
      // Strip npm scope: @scope/foo → foo
      return pkg.name.replace(/^@[^/]+\//, "");
    }
  } catch {
    // No package.json or invalid JSON — fall through
  }
  // Fall back to directory name
  return path.basename(cwd);
}

function parseCliArgs(): CliOptions {
  const { values } = parseArgs({
    options: {
      source: { type: "string", short: "s" },
      output: { type: "string", short: "o" },
      timeout: { type: "string", short: "t" },
      disable: { type: "string", short: "d" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
  });

  if (values.help) {
    console.log(`Usage: oprc-compile [options]

Options:
  -s, --source <path>     Source file (default: src/index.ts)
  -o, --output <path>     Output WASM file (default: dist/<name>.wasm)
  -t, --timeout <ms>      Compilation timeout in ms (default: 120000)
  -d, --disable <features>  Comma-separated WASI features to disable
                             (stdio, random, clocks, http, fetch-event)
  -h, --help              Show this help message`);
    process.exit(0);
  }

  let disableFeatures: DisableFeature[] = [];
  if (values.disable) {
    const features = values.disable.split(",").map((f) => f.trim());
    for (const f of features) {
      if (!VALID_DISABLE_FEATURES.has(f)) {
        console.error(`Unknown disable feature: "${f}"`);
        console.error(`Valid features: ${[...VALID_DISABLE_FEATURES].join(", ")}`);
        process.exit(1);
      }
    }
    disableFeatures = features as DisableFeature[];
  }

  return {
    source: values.source ?? "src/index.ts",
    output: values.output ?? "", // resolved later from project name
    timeout: values.timeout ? parseInt(values.timeout, 10) : 120_000,
    disableFeatures,
  };
}

async function main(): Promise<void> {
  const cwd = process.cwd();
  const opts = parseCliArgs();

  // Resolve source path
  const sourcePath = path.resolve(cwd, opts.source);

  // Resolve output path
  let outputPath: string;
  if (opts.output) {
    outputPath = path.resolve(cwd, opts.output);
  } else {
    const name = await readProjectName(cwd);
    outputPath = path.resolve(cwd, "dist", `${name}.wasm`);
  }

  // Resolve WIT and SDK paths from the compiler's own location
  // In dist/: __dirname = <compiler>/dist → go up one level to <compiler>
  // Then resolve relative to the monorepo layout
  const witPath = resolveDefaultPath("../../../data-plane/oprc-wasm/wit");
  const sdkPath = resolveDefaultPath("../../oaas-sdk-ts/src");

  // Read source
  let source: string;
  try {
    source = await fs.readFile(sourcePath, "utf-8");
  } catch {
    console.error(`Could not read source file: ${sourcePath}`);
    process.exit(1);
  }

  console.log(`Source:  ${sourcePath}`);
  console.log(`Output:  ${outputPath}`);
  console.log(`WIT:     ${witPath}`);
  console.log(`SDK:     ${sdkPath}`);
  if (opts.disableFeatures.length > 0) {
    console.log(`Disable: ${opts.disableFeatures.join(", ")}`);
  }
  console.log();
  console.log("Compiling...");

  const startTime = Date.now();
  const result = await compileTypeScript(
    source,
    witPath,
    sdkPath,
    opts.timeout,
    opts.disableFeatures.length > 0 ? opts.disableFeatures : undefined,
  );
  const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);

  if (!result.success) {
    console.error(`Compilation failed after ${elapsed}s:`);
    for (const error of result.errors) {
      console.error(`  - ${error}`);
    }
    process.exit(1);
  }

  console.log(`Compiled in ${elapsed}s`);
  console.log(`  WASM Component size: ${(result.component.byteLength / 1024 / 1024).toFixed(2)} MB`);

  // Verify WASM magic bytes
  const magic = result.component.slice(0, 4);
  const magicOk = magic.every((b, i) => b === WASM_MAGIC[i]);
  if (!magicOk) {
    console.error("  WASM magic bytes: INVALID — output may be corrupted");
    process.exit(1);
  }

  // Write output
  const outDir = path.dirname(outputPath);
  await fs.mkdir(outDir, { recursive: true });
  await fs.writeFile(outputPath, result.component);
  console.log(`  Written to ${outputPath}`);
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
