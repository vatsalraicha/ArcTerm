#!/usr/bin/env node
/**
 * Import Fig autocomplete specs (withfig/autocomplete, MIT) and emit a
 * static JSON registry that ArcTerm's Rust backend compiles in.
 *
 * Why AST extraction instead of dynamic import:
 *   - Fig specs are TypeScript that imports runtime helpers like
 *     `@fig/autocomplete-generators`. Running them in Node would mean
 *     resolving and stubbing those imports — fragile.
 *   - We don't need Fig's executable bits (generators, postProcess,
 *     parserDirectives). Those require either a JS runtime inside
 *     ArcTerm or hand-porting. Out of scope for Phase 7.
 *   - The AST walk keeps only static literal fields (name, description,
 *     subcommand/option trees). Pure data. Serialization is trivial.
 *
 * Output: one JSON file per command in
 *   apps/desktop/src-tauri/completion-specs/<command>.json
 * plus an index.json that lists every command + its file path.
 *
 * Usage:
 *   cd scripts
 *   npm install           # installs typescript
 *   npm run import-fig
 */

import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const FIG_SRC = path.join(REPO_ROOT, ".fig-autocomplete-source/src");
const OUT_DIR = path.join(REPO_ROOT, "apps/desktop/src-tauri/completion-specs");

// How many top-level spec files to import. Set to null to pull the
// entire withfig/autocomplete set (~735 commands, ~25-30 MB bundle).
// A non-null ALLOWLIST restores curation if we ever need to bound
// binary size again. We default to the full set now that the pipeline
// is stable enough to handle the long tail.
const ALLOWLIST = null;

async function main() {
    // Sanity check: does the Fig source exist? If not, print a friendly
    // message instead of an ENOENT stack trace — the most common setup
    // mistake is forgetting to clone the source first.
    try {
        await fs.access(FIG_SRC);
    } catch {
        console.error(
            `Fig source not found at ${FIG_SRC}\n` +
                `Run: git clone --depth 1 https://github.com/withfig/autocomplete.git .fig-autocomplete-source`,
        );
        process.exit(1);
    }

    await fs.mkdir(OUT_DIR, { recursive: true });

    // Keyed by the canonical command name.
    const index = {};
    let emitted = 0;
    let skipped = 0;
    let empty = 0;

    // Resolve the list of spec files to import. ALLOWLIST=null => walk
    // the entire src/ directory; otherwise stick to the hand-picked set.
    const targets = ALLOWLIST
        ? ALLOWLIST.map((n) => ({ name: n, srcFile: path.join(FIG_SRC, `${n}.ts`) }))
        : (await fs.readdir(FIG_SRC))
              .filter((f) => f.endsWith(".ts") && !f.startsWith("-"))
              .map((f) => ({
                  name: f.replace(/\.ts$/, ""),
                  srcFile: path.join(FIG_SRC, f),
              }));

    // Quiet log when importing the full tree (>100 files); allowlist mode
    // still uses the detailed per-entry log so curation stays legible.
    const verbose = !!ALLOWLIST || targets.length < 100;

    for (const { name, srcFile } of targets) {
        let source;
        try {
            source = await fs.readFile(srcFile, "utf8");
        } catch {
            if (verbose) console.warn(`  ! no Fig spec for '${name}' (skipping)`);
            skipped++;
            continue;
        }

        const spec = extractSpec(source, srcFile);
        if (!spec) {
            if (verbose) console.warn(`  ! could not parse '${name}' (skipping)`);
            skipped++;
            continue;
        }

        const normalized = normalizeSpec(spec);
        const subc = countSubcommands(normalized);
        const opts = countOptions(normalized);

        // Drop completely-empty specs. They show up occasionally (abandoned
        // stubs in the Fig repo); keeping them just wastes binary size for
        // no completion value.
        if (subc === 0 && opts === 0) {
            empty++;
            continue;
        }

        index[name] = normalized;
        emitted++;
        if (verbose) {
            console.log(`  ✓ ${name.padEnd(14)} ${subc} subcommands, ${opts} options`);
        }
    }

    // One combined bundle — simpler include_str!-based load on the Rust
    // side, single file to version and review. We pretty-print so the
    // diff view in a PR is legible when a spec changes.
    const bundlePath = path.join(OUT_DIR, "bundle.json");
    await fs.writeFile(bundlePath, JSON.stringify(index, null, 2));

    const bytes = (await fs.stat(bundlePath)).size;
    console.log(
        `\nDone: ${emitted} specs emitted, ${skipped} skipped, ${empty} empty.\n` +
            `Output: ${bundlePath} (${(bytes / 1024).toFixed(0)} KB / ${(bytes / 1_048_576).toFixed(1)} MB)`,
    );
}

// ---------------------------------------------------------------------
// AST walker: turn a TS object literal into a plain JS value, dropping
// anything non-literal (functions, identifiers, member expressions, etc).
// ---------------------------------------------------------------------

/**
 * Locate the `completionSpec` variable initializer in a source file and
 * return it as a plain JS object, or null if we can't find/parse it.
 *
 * Fig specs come in a few forms:
 *
 *   const completionSpec = { ... };            // plain object
 *   const completionSpec = () => ({ ... });    // arrow returning object
 *   const completionSpec = () => { return {...}; };  // arrow with block body
 *
 * The function forms exist so the spec can take configuration args at
 * `export default completionSpec(...)` time. We're extracting the default
 * shape, so we walk into the arrow's return value.
 */
function extractSpec(source, filename) {
    const sf = ts.createSourceFile(
        filename,
        source,
        ts.ScriptTarget.Latest,
        true,
    );
    let initializer = null;
    ts.forEachChild(sf, (node) => {
        if (initializer) return;
        if (!ts.isVariableStatement(node)) return;
        for (const decl of node.declarationList.declarations) {
            if (!decl.name || !ts.isIdentifier(decl.name)) continue;
            if (decl.name.text !== "completionSpec") continue;
            if (!decl.initializer) continue;
            initializer = decl.initializer;
            return;
        }
    });
    if (!initializer) return null;
    const objNode = unwrapToObjectLiteral(initializer);
    if (!objNode) return null;
    return literalToJs(objNode);
}

/**
 * Unwrap arrow/function wrappers to get at the object literal inside.
 * Returns null if no object literal is reachable.
 */
function unwrapToObjectLiteral(node) {
    if (ts.isObjectLiteralExpression(node)) return node;
    if (ts.isParenthesizedExpression(node)) {
        return unwrapToObjectLiteral(node.expression);
    }
    if (ts.isArrowFunction(node) || ts.isFunctionExpression(node)) {
        // Concise body: `() => ({...})`
        if (!ts.isBlock(node.body)) {
            return unwrapToObjectLiteral(node.body);
        }
        // Block body: walk statements looking for the first `return X`.
        for (const stmt of node.body.statements) {
            if (ts.isReturnStatement(stmt) && stmt.expression) {
                return unwrapToObjectLiteral(stmt.expression);
            }
        }
    }
    return null;
}

/**
 * Convert a TypeScript AST node into a plain JS value. Recognized:
 *   ObjectLiteralExpression -> object
 *   ArrayLiteralExpression  -> array
 *   StringLiteral           -> string
 *   NumericLiteral          -> number
 *   TrueKeyword/FalseKeyword -> bool
 *   NullKeyword             -> null
 *   NoSubstitutionTemplateLiteral -> string
 * Anything else (ArrowFunction, CallExpression, Identifier referencing
 * an external helper, etc) collapses to `undefined` and is filtered out
 * of parent objects/arrays.
 */
function literalToJs(node) {
    if (ts.isObjectLiteralExpression(node)) {
        const obj = {};
        for (const prop of node.properties) {
            if (!ts.isPropertyAssignment(prop)) continue;
            const key = propertyNameText(prop.name);
            if (!key) continue;
            const v = literalToJs(prop.initializer);
            if (v !== undefined) obj[key] = v;
        }
        return obj;
    }
    if (ts.isArrayLiteralExpression(node)) {
        const arr = [];
        for (const el of node.elements) {
            const v = literalToJs(el);
            if (v !== undefined) arr.push(v);
        }
        return arr;
    }
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
        return node.text;
    }
    if (ts.isNumericLiteral(node)) {
        return Number(node.text);
    }
    if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
    if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
    if (node.kind === ts.SyntaxKind.NullKeyword) return null;
    // Spread elements in arrays are also non-literal — fine to drop.
    // Template strings with interpolation also drop (can't resolve the
    // substituted values statically).
    return undefined;
}

function propertyNameText(name) {
    if (!name) return null;
    if (ts.isIdentifier(name)) return name.text;
    if (ts.isStringLiteral(name)) return name.text;
    if (ts.isNumericLiteral(name)) return name.text;
    return null;
}

// ---------------------------------------------------------------------
// Normalization: align Fig's shapes with the JSON schema our Rust side
// deserializes. Removes AST leftovers that aren't useful to ArcTerm.
// ---------------------------------------------------------------------

function normalizeSpec(spec) {
    return {
        names: namesOf(spec),
        description: stringField(spec.description),
        subcommands: normalizeSubcommands(spec.subcommands),
        options: normalizeOptions(spec.options),
    };
}

function normalizeSubcommands(list) {
    if (!Array.isArray(list)) return [];
    return list
        .map((entry) => {
            if (!entry || typeof entry !== "object") return null;
            const names = namesOf(entry);
            if (names.length === 0) return null;
            return {
                names,
                description: stringField(entry.description),
                subcommands: normalizeSubcommands(entry.subcommands),
                options: normalizeOptions(entry.options),
            };
        })
        .filter(Boolean);
}

function normalizeOptions(list) {
    if (!Array.isArray(list)) return [];
    return list
        .map((entry) => {
            if (!entry || typeof entry !== "object") return null;
            const names = namesOf(entry);
            if (names.length === 0) return null;
            return {
                names,
                description: stringField(entry.description),
            };
        })
        .filter(Boolean);
}

function namesOf(obj) {
    if (!obj) return [];
    const n = obj.name;
    if (typeof n === "string") return [n];
    if (Array.isArray(n)) return n.filter((s) => typeof s === "string");
    return [];
}

function stringField(v) {
    return typeof v === "string" ? v : null;
}

function countSubcommands(spec) {
    if (!spec || !Array.isArray(spec.subcommands)) return 0;
    return spec.subcommands.length;
}

function countOptions(spec) {
    if (!spec || !Array.isArray(spec.options)) return 0;
    return spec.options.length;
}

function safeFilename(name) {
    return name.replace(/[^A-Za-z0-9._-]+/g, "_");
}

main().catch((err) => {
    console.error(err);
    process.exit(1);
});
