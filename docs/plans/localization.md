# Plan: multilingual user interface

- **Status:** planned, not started
- **Written:** 2026-10-26
- **Goal:** the UI is available in several languages, starts in the system language and
  can be switched in the app. English and Polish first; adding a language later means
  adding one translation file, with no code changes.
- **Constraints:** portable (same mechanism on Windows, macOS and Linux) and light (no
  large i18n framework, no noticeable effect on start time or binary size).

## Current state

- All UI text is hard-coded Polish: about 55 strings in `app.rs`, `view.rs`, `worker.rs`,
  `print.rs` and `main.rs` (tooltips, buttons, status messages, error screens, welcome
  screen, search counter).
- The worker produces finished Polish error messages (`worker.rs`, mapping PDFium errors),
  so translation cannot happen in the UI layer today.
- Plural forms are not handled: the search counter shows `"{n} wyników"` for any `n`,
  which is wrong in Polish for 1 ("1 wynik") and 2–4 ("2 wyniki").
- Outside the app: the installer already offers Polish and English wizard languages, but
  the file type name ("Dokument PDF") and app description ("Lekki czytnik PDF") written to
  the registry by the installer and `register.ps1` are Polish only.
- System dialogs (file open via `rfd`, the Windows print dialog) are already localized
  by the OS.

## Design

### Translation files

- One file per language in `locales/` (`en.toml`, `pl.toml`), embedded into the binary
  with `include_str!`. Translations ship inside the exe, so the portable build stays
  a single executable plus `pdfium.dll`.
- Flat keys grouped by area, e.g.:

  ```toml
  [toolbar]
  open = "Open (Ctrl+O)"
  print = "Print (Ctrl+P)"

  [search]
  hits = { one = "{n} result", other = "{n} results" }
  hit_of = "{current} of {n}"
  ```

- Placeholders are named (`{n}`, `{page}`, `{error}`), so word order can differ between
  languages.
- English is the source and fallback language: a key missing in another language falls
  back to English. A unit test checks that every language file has exactly the keys of
  `en.toml` and the same placeholders.

### Lookup in code

- A small `i18n` module (~150 lines): parses the active language at startup into a map,
  exposes `tr("toolbar.open")` and `tr_args("search.hit_of", &[("current", ..), ("n", ..)])`
  and `tr_count("search.hits", n)`.
- Plural rules are a small hand-written function per language (English: one/other;
  Polish: one/few/many; more languages add their rule). No `fluent` or ICU dependency:
  they are much larger than this app needs.
- Keyboard shortcut labels stay out of translations and are appended in code, so they
  cannot drift from the real shortcuts (and can later show ⌘ on macOS).

### Language selection

- Default: the system UI language via the `sys-locale` crate (tiny, works on Windows,
  macOS and Linux), mapped to the closest available translation, otherwise English.
- Override: a language picker in the app (toolbar menu), saved as `language` in
  `settings.toml`. Switching applies immediately, without restart.

### Errors from the worker

- `worker.rs` sends error kinds (`InvalidPdf`, `UnsupportedSecurity`, `NeedsPassword`,
  `NotOpen`, `Other(String)`) instead of finished sentences; `print.rs` does the same.
  The UI translates them. Raw technical details stay available for an "Other" fallback.

### Fonts and scripts

- Latin, Greek and Cyrillic work with the current fonts (Segoe UI / SF / DejaVu).
- CJK (Chinese, Japanese, Korean) needs a system font fallback loaded only when such a
  language is active (e.g. Microsoft YaHei / Yu Gothic / Malgun Gothic on Windows,
  PingFang / Hiragino on macOS, Noto CJK on Linux). Bundling a CJK font is not an option
  (tens of MB).
- Right-to-left languages (Arabic, Hebrew) are out of scope: egui has no RTL layout.

### Outside the app

- Installer: the file type name and app description come from `[CustomMessages]` in the
  selected wizard language instead of being hard-coded Polish.
- `register.ps1`: English strings by default, or picked from the Windows UI language.
- README: list the available languages and how to add one (copy `en.toml`, translate,
  add the plural rule, open a pull request).

## Steps

1. `i18n` module with English fallback, plural rules for `en` and `pl`, key consistency
   test.
2. Move all UI strings to `locales/en.toml` and `locales/pl.toml`; replace literals in code.
   Fixes the Polish plural forms in the search counter on the way.
3. Error kinds instead of finished messages in `worker.rs` and `print.rs`.
4. System language detection (`sys-locale`), language picker, `language` in settings.
5. Installer and `register.ps1` strings per language; README section for translators.
6. Later, per language request: CJK font fallback.

## Estimate

About 300–400 lines of code plus the two translation files. Steps 1–2 are the bulk and
are mechanical; the only design-sensitive part is the plural and placeholder handling.
Binary size impact is a few kilobytes per language.

## Verification

- Unit tests: key and placeholder consistency across languages, plural rules
  (Polish 1 / 2 / 5 / 12 / 22 / 25).
- Manual: start with Windows set to English and to Polish, switch the language at runtime,
  check that no Polish text remains in English mode and that long English/Polish strings
  fit the toolbar and bars.
