import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const CHECK_ONLY = process.argv.includes("--check");

const packageJsonPath = resolve(import.meta.dir, "../package.json");
const cargoTomlPath = resolve(import.meta.dir, "../synapse/Cargo.toml");

const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf-8"));
const version: string = packageJson.version;

const cargoToml = readFileSync(cargoTomlPath, "utf-8");
const versionRegex = /^version\s*=\s*"[^"]*"/m;
const match = cargoToml.match(versionRegex);

if (!match) {
  console.error("Could not find version field in synapse/Cargo.toml");
  process.exit(1);
}

const currentCargoVersion = match[0].match(/"([^"]*)"/)?.[1];

if (currentCargoVersion === version) {
  console.log(`Versions in sync: ${version}`);
  process.exit(0);
}

if (CHECK_ONLY) {
  console.error(
    `Version mismatch: package.json=${version}, Cargo.toml=${currentCargoVersion}`,
  );
  process.exit(1);
}

const updated = cargoToml.replace(versionRegex, `version = "${version}"`);
writeFileSync(cargoTomlPath, updated);
console.log(`Synced version to ${version} in synapse/Cargo.toml`);
