# Tamagui maintenance and TypeScript compatibility

Last checked: 2026-09-09. This is a recorded dependency follow-up, not an
automatically scheduled release monitor.

## TypeScript 7 blocker

Clipper uses the Go-based `tsgo` executable for type checking. Tamagui's static
extractor separately imports the JavaScript TypeScript compiler API to read
tsconfig files and resolve aliases. TypeScript 7 does not expose the API used by
this code; configuration bundling fails at `sys.fileExists`, followed by
misleading `Must provide components` errors.

The `pnpm-workspace.yaml` package extension supplies `@tamagui/static@2.7.7`
with TypeScript `~5.9.2` (locked to 5.9.3), matching its upstream development
dependency. It does not replace Clipper's type checker or ship TypeScript in the
browser bundle.

Checked upstream npm distribution tags and the published extractor code:

| Channel | Version             | Extractor still uses legacy TypeScript API |
| ------- | ------------------- | ------------------------------------------ |
| latest  | 2.7.7               | Yes                                        |
| canary  | 2.7.7-1788328229285 | Yes                                        |
| beta    | 3.0.0-beta.1123.1   | Yes                                        |

The default branch's
[esbuildTsconfigPaths.ts](https://github.com/tamagui/tamagui/blob/64a9282e/code/compiler/static/src/extractor/esbuildTsconfigPaths.ts)
also still imports `sys`, `findConfigFile`, `readConfigFile`,
`parseJsonConfigFileContent`, and `nodeModuleNameResolver` from `typescript`.

[PR #4171](https://github.com/tamagui/tamagui/pull/4171), merged September 3,
guards `ts.sys` in `@tamagui/build` so `--skip-types` can run on TypeScript 7.
It does not change the static extractor and explicitly does not support full
type builds on TypeScript 7. Do not mistake its appearance in future release
notes for a fix to Clipper's failure.

## When upgrading

1. Check the published `@tamagui/static` package, not only release notes, for
   removal/replacement of those legacy compiler API calls, or a correctly
   declared compatible runtime dependency.
2. Upgrade the directly pinned Tamagui packages together. Remove or update the
   version-scoped package extension only after checking the new dependency graph.
3. Verify installation from the frozen lockfile, Vite development transforms of
   `main.tsx`, `App.tsx`, and `SchedulePanel.tsx`, and the production web build.
   Inspect logs: Tamagui can report extraction errors while Vite exits successfully.
4. Run web/mobile checks and check rendered themes, responsive styles, and
   native layouts. Confirm Clipper's type-check commands still use `tsgo`.

## Maintenance assessment

The project is actively maintained, with evidence beyond version churn:

- [Stable 2.7.7](https://github.com/tamagui/tamagui/releases/tag/v2.7.7) was
  published August 15. The preceding August releases fixed conditional media
  extraction, offscreen measurements, and interrupted sheet animations.
- [Recent default-branch commits](https://github.com/tamagui/tamagui/commits/main/)
  include September 3 fixes for web slider coordinates, forwarded refs, theme
  declaration order, RTL styling, and per-worker compiler temporary files.
  Contributions include Nate Wienert, Mad Dinh, Jordan Hayashi, and Janic Duplessis.
- [Native Detox tests](https://github.com/tamagui/tamagui/actions/runs/34342496777)
  and [iOS Maestro tests](https://github.com/tamagui/tamagui/actions/runs/34342496700)
  completed successfully September 9. These are evidence of ongoing testing,
  not proof of every supported configuration's correctness.

Assessment: keep Tamagui for Clipper, but budget for build-tool compatibility
work and test upgrades. The missing compiler dependency is a real packaging
defect, and merged fixes are not necessarily in the latest stable release.
Active maintenance alone does not establish consistently quick issue resolution
or justify moving this QA branch to a beta release.
