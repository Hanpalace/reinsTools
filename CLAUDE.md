# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

再保运维工具 — a cross-platform desktop app template built on **Tauri 2** (Rust backend) + **Vue 3 / Vite 6 / TypeScript** frontend, with Naive UI, UnoCSS, Pinia, vue-router, vue-i18n, and axios. The app has a home page, a 批量导入 (bulk data import) scaffold page, and a 数据连接配置 CRUD page (DB connection test/save/list/delete backed by the Rust sqlx module); the value is the template: dual layouts, theming, i18n, an axios wrapper, and platform system-method wrappers.

## Commands

Package manager is **pnpm v12**. There is no lint config, test suite, or CI.

- `pnpm install` — install dependencies. esbuild's build script must stay allowed in [pnpm-workspace.yaml](pnpm-workspace.yaml), otherwise install fails with `ERR_PNPM_IGNORED_BUILDS`.
- `pnpm dev` — Vite dev server only, http://localhost:1420 (strict port). Opens the UI in a browser; **Tauri APIs are unavailable in this mode**.
- `pnpm tauri dev` — full desktop app. Runs `pnpm dev` itself (`beforeDevCommand`), compiles the Rust backend, and opens the app window. First compile takes several minutes; closing the window stops everything (exits code 0 — normal, not a crash).
- `pnpm build` — production frontend build (uses `.env.production`).
- `pnpm build:test` — test-mode frontend build (`vite build --mode test`, uses `.env.test`).
- `pnpm tauri build` — package a release installer (Windows output: `src-tauri/target/release/bundle/nsis/`).
- Typecheck (no script wired up): `npx vue-tsc --noEmit`

## Prerequisites

- Rust ≥ 1.77.2 (Tauri 2 MSRV). This machine's toolchain was updated from 1.68.2 to 1.98.1 for this reason.
- Node LTS and pnpm ≥ 12.
- Cargo uses the USTC crates.io mirror: [src-tauri/.cargo/config.toml](src-tauri/.cargo/config.toml).

## Environment variables (Vite)

Loaded by mode: `.env` (dev), `.env.production` (`pnpm build`), `.env.test` (`--mode test`).

- `VITE_APP_TITLE` — app title text; the single brand-name source: `index.html` uses `%VITE_APP_TITLE%`, and layouts/HomeView read `import.meta.env.VITE_APP_TITLE`
- `VITE_API_BASE_URL` — axios baseURL (see [src/utils/request.ts](src/utils/request.ts))
- `VITE_API_TIMEOUT` — axios timeout in ms
- `VITE_DEFAULT_LOCALE` — initial i18n locale: `zh-CN` or `en`

## Architecture

### Frontend ([src/](src/))

- [src/main.ts](src/main.ts) — entry; installs Pinia, router, vue-i18n, mounts `App`.
- [src/App.vue](src/App.vue) — root: Naive UI `n-config-provider` (theme follows the theme store; locale follows vue-i18n) wrapping message/notification/dialog/loading-bar providers. Renders `MainLayout` or `TopMenuLayout` depending on the layout store.
- **Layouts**: [src/layouts/MainLayout.vue](src/layouts/MainLayout.vue) (sidebar via `n-layout-sider` + `n-menu`) and [src/layouts/TopMenuLayout.vue](src/layouts/TopMenuLayout.vue). Both consume the shared menu from [src/composables/useAppMenu.ts](src/composables/useAppMenu.ts); `LayoutSwitcher.vue` toggles them via the layout store.
- **Menu** ([src/composables/useAppMenu.ts](src/composables/useAppMenu.ts)) — single source of menu options. Leaf items carry a `path`; `navigateByMenuKey` pushes it verbatim and ignores parent keys (no key→path string surgery).
- **Router** ([src/router/index.ts](src/router/index.ts)): 4 routes — home plus lazy-loaded `/data/batch-import`, `/data/connection`, `/data/import-log`. The 数据批量导入 menu is a parent with three children.
- **Stores** ([src/stores/](src/stores/)): setup-style Pinia stores. `theme.ts` holds an in-memory `isDark` toggle (no persistence); `layout.ts` holds `currentLayout: 'sider' | 'top'`.
- **i18n** ([src/locales/index.ts](src/locales/index.ts)): Composition mode (`legacy: false`), locale from `VITE_DEFAULT_LOCALE`, fallback `en`. Exports helpers `t()` (non-reactive, for setup) and `useAppI18n()` (reactive), plus `supportLanguages` which drives `LanguageSwitcher.vue`.
- [src/sys-methods/](src/sys-methods/) — platform-abstraction wrappers (network, bluetooth, system, notification, autostart, file), re-exported by its `index.ts`. Most shell out via `@tauri-apps/plugin-shell` `Command` with a `platform()` branch (`windows` vs `macos`). They only work inside Tauri — not under plain `pnpm dev`. No view currently imports them — kept as reusable infrastructure.
- [src/utils/request.ts](src/utils/request.ts) — shared axios instance; the response interceptor unwraps `response.data` and shows a Naive UI toast on error.

**Auto-imports**: `unplugin-auto-import` provides Vue APIs and Naive UI `useDialog`/`useMessage`/`useNotification`/`useLoadingBar` globally; `unplugin-vue-components` with `NaiveUiResolver` registers all `n-*` components **and** auto-registers components under `src/components/` (e.g. `QueryTable` is used without an import — `components.d.ts` records it). The generated `auto-imports.d.ts` / `components.d.ts` are checked in and regenerated on dev/build.

**Shared components**: [src/components/QueryTable.vue](src/components/QueryTable.vue) — the standard 查询列表 card (centered headers, zebra/hover rows, empty state, count + pagination; page resets to 1 on page-size change). Columns via `columns` prop; custom cells via per-column-key scoped slots.

**Styling**: UnoCSS ([uno.config.ts](uno.config.ts)) — `presetUno({ dark: 'class' })`, `presetAttributify`, `presetIcons`. Icons are used as CSS classes in markup, e.g. `i-carbon-home`. The `dark:` variant follows the `dark` class on `<html>`, toggled by `stores/theme.ts` in sync with the Naive UI theme.

### Tauri backend ([src-tauri/](src-tauri/))

- [src-tauri/src/lib.rs](src-tauri/src/lib.rs) — a standard `tauri::Builder` registering plugins (opener, shell, notification, dialog, autostart, os) plus one demo `greet` command and the four DB commands from `db`. New custom commands go here, registered in `invoke_handler`.
- [src-tauri/src/db.rs](src-tauri/src/db.rs) — DB connection module (sqlx): commands `test_connection`, `save_connection`, `list_connections`, `delete_connection`. Configs persist to `{app_config_dir}/connections.json` (plaintext passwords — accepted tradeoff for a local tool). Each config carries a stable `id` (upsert/delete keyed on it); `save`/`delete` return the updated list; read-modify-write is serialized by a tokio Mutex; file I/O uses `tokio::fs`.
- [src-tauri/src/import.rs](src-tauri/src/import.rs) — batch import console (migrated from MySQL_Batch_Import_Tool): `stat_import_files`, `preview_import` (parse + history check + delete dry-run), `execute_import` (single transaction: deletes → merged batch INSERTs → row-count check vs rows_affected → commit/rollback), `export_import_script` (writes merged SQL to .sql files for offline/intranet use, splittable by `max_part_bytes`; connection optional). Progress pushed via `app.emit("import-progress")` events. Import files are parsed line-per-INSERT with UTF-8/GB18030 fallback; constraints stay ON (bad data fails the transaction); `strip_auto_increment` (default on) strips target-table auto-increment columns via information_schema. Unit tests live in the module.
- [src-tauri/src/history.rs](src-tauri/src/history.rs) — import history ledger in SQLite (`{app_config_dir}/import_history.db`, WAL; one-time migration from the legacy `import_history.json`, archived as `.migrated`). Records per-file rows grouped by `run_id` (shared per import run); `run_meta` table holds per-run remark + `config_json` (ReplayConfig: file paths, connection id, delete script, options — enables one-click re-execution from the history list). Commands/fns: `list_import_history` (filtered runs viewer: keyword/ok/time-range), `get_history_run` (single run for replay), `add_history_run` / `update_history_run` / `delete_history_run` (manual CRUD for off-app imports), `find_hits` (re-import guard by sha256), `record_run` (called after COMMIT).
- [src-tauri/src/runlog.rs](src-tauri/src/runlog.rs) — append-only run log (`{app_config_dir}/import_log.txt`, rotates at 5MB to `.1`); commands `read_import_log` (tail N lines), `clear_import_log`. Log lines: `[UTC时间] [阶段] 内容`. Timestamps computed via civil-from-days (no chrono dep).
- [src-tauri/capabilities/default.json](src-tauri/capabilities/default.json) — permission whitelist. `shell:allow-execute` enumerates exactly which system commands the frontend may run (`netsh`, `networksetup`, `powershell`, `open`, `getmac`). **Any new command invoked from `sys-methods/` must be added here or it fails at runtime.**
- [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json) — `beforeDevCommand: pnpm dev`, `devUrl: http://localhost:1420`, `frontendDist: ../dist`, window "再保运维工具" 1440×810. To package a test-mode build, temporarily change `beforeBuildCommand` to `pnpm build:test`.

## Gotchas

- pnpm 12 ignores the `pnpm` field in `package.json`; the build-script allowlist lives in `pnpm-workspace.yaml` (`allowBuilds`). Don't "fix" it by moving the setting back into `package.json`.
- Icons are wired explicitly: `uno.config.ts` supplies the `carbon` collection from `@iconify-json/carbon`. It must use the **function form** (`carbon: () => carbonIcons`) — passing a plain IconifyJSON doesn't resolve under `@iconify/utils` 3.x. The monolithic `@iconify/json` was removed; new icon sets should be added as `@iconify-json/<set>`.
- System methods (WiFi toggle, …) may need elevated privileges and can fail silently under `tauri dev`; the README recommends testing them in a packaged build run as administrator.
- Closing the app window ends `pnpm tauri dev` with exit code 0 — expected behavior.
