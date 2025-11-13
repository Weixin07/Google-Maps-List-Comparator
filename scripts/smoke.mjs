#!/usr/bin/env node
import { spawnSync } from "node:child_process";

const steps = [
  { label: "Lint", command: "pnpm lint" },
  { label: "Typecheck", command: "pnpm typecheck" },
  { label: "Vitest (CI profile)", command: "pnpm test:ci" },
  {
    label: "Rust unit tests",
    command: "cargo test -p tauri-app --manifest-path src-tauri/Cargo.toml",
  },
];

const skipJs = process.env.SMOKE_SKIP_JS === "1";
const skipRust = process.env.SMOKE_SKIP_RUST === "1";

const filtered = steps.filter((step) => {
  if (skipJs && step.command.startsWith("pnpm")) {
    return false;
  }
  if (skipRust && step.command.startsWith("cargo")) {
    return false;
  }
  return true;
});

if (filtered.length === 0) {
  console.log("[smoke] No steps to run (all skipped).");
  process.exit(0);
}

for (const step of filtered) {
  console.log(`\n[smoke] ${step.label} :: ${step.command}`);
  const result = spawnSync(step.command, {
    stdio: "inherit",
    shell: true,
    env: {
      ...process.env,
      FORCE_COLOR: "1",
    },
  });
  if (result.status !== 0) {
    console.error(`[smoke] Step failed: ${step.label}`);
    process.exit(result.status ?? 1);
  }
}

console.log("\n[smoke] All checks passed");
