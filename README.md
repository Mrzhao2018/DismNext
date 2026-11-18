# DismNext

Clean-room modernized Windows system cleanup and optimization suite built with **Rust** (`dism-core`, `dism-cli`, `dism-app`) and **React + Microsoft Fluent UI 9** (`ui/`).

## Structure

- `catalog/rules.json` — Compiled bilingual cleanup & optimization catalog (86 cleanup rules, 21 groups / 175 optimization items, compiled via `scripts/build_catalog.py` with Windows 11 maintenance & optimization definitions).
- `crates/dism-core` — Headless engine: condition evaluator, parallel file scanner with Dry-Run lock/ACL diagnostics, registry optimizer, and automatic pre-mutation snapshot rollback.
- `crates/dism-cli` — Command-line interface (`info`, `scan`, `preview`, `clean`, `opt`, `snapshot`).
- `crates/dism-app` — Local REST API server & desktop host launcher (`msedge --app`).
- `ui/` — Windows 11 Fluent UI 9 frontend (Vite + React + TypeScript).

## Build & Run

```powershell
# 1. Build frontend
Set-Location ui
npm install
npm run build
Set-Location ..

# 2. Run Rust tests & build release binaries
cargo test --workspace
cargo build --release --workspace

# 3. Launch desktop UI (run elevated for HKLM / system cleanup)
.\target\release\dism-app.exe

# Or use CLI
.\target\release\dism-cli.exe scan
```
