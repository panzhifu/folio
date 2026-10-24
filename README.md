[中文](./doc/README_zh.md) | English

<p align="center"><img src="packaging/icons/folio.png" width="112" alt="Folio icon"></p>

<h1 align="center">Folio</h1>

<p align="center">
  <strong>A native GPU e-book reader for desktop and Android, written in Rust.</strong><br>
  No WebView, no browser engine — every page is typeset by Folio's own layout engine.
</p>

---

Folio opens books in a GPU-rendered native window and lays the text out itself:
a DOM → CSS-subset → layout pipeline inside `folio-core`, built on GPUI's text
primitives (shaping, line breaking, per-glyph painting) and the application shell
from [gpui-kit](https://gpui-kit.com). Doing the typesetting in-house is what makes
book typography possible without shipping a browser: first-line indents, centered
chapter titles, mixed text runs, images that respect `width` / `max-width`,
footnotes, vertical writing.

## Highlights

- **An engine that knows books** — reflowable and fixed-layout documents share one
  anchor system; reading positions survive font-size and window-resize changes
  because they are chapter-internal character offsets, never page numbers.
- **21 formats, 30 file extensions** — see the table below.
- **Three reading modes** — continuous scroll, paged, and two-page spread (it steps
  down automatically when the window is too narrow). Fixed-layout books get a
  page-by-page mode with zoom, share and save.
- **A library that remembers** — shelf groups, per-book progress, reading
  statistics, and full-text search inside a book.
- **9 interface languages** — 简体中文 · 繁體中文 · English · 日本語 · 한국어 ·
  Français · Deutsch · Español · Русский.
- **Small native packages on four platforms** — Linux, macOS, Windows, Android.

## Supported formats

| State | Formats |
|---|---|
| Reflowable, fully readable | EPUB (incl. fixed-layout), MOBI / AZW / AZW3 / PRC (KF7 & KF8, incl. hybrid files), FB2, TXT, Markdown, HTML / MHTML / HTMLZ, RTF, DOCX, legacy DOC, ODT, PDB / PalmDoc, CHM |
| Fixed-layout, page-by-page | PDF, AZW4 (PalmDB-embedded PDF), DjVu (pure-Rust decoder, both container shapes; NAVM bookmarks become the TOC), CBZ / CB7 / CBR / CBT (CBR via a pure-Rust RAR reader) |
| Partial | PDF / AZW4 / DjVu have **no text layer yet** — selection, annotations and search do not apply inside them. Word lists driven by styles (and per-section restart numbering) and ODT `text:continue-numbering` are not followed |
| Not supported | DRM-protected books (a DRM-flagged PDB is detected and reported, not decrypted), encrypted RAR archives |

Notes:

- Comics: a `ComicInfo.xml` in the package (as written by ComicRack, CDisplayEx or
  Calibre) fills in series, number, credits and summary; a `folder.jpg` /
  `cover.jpg` is treated as the **cover**, not as a phantom first page.
- Word / ODT: lists follow the spec's numbering, footnotes and endnotes render as
  tappable superscripts with the note body back in the chapter that cites them.

## Download

Packages are published on the
[Releases](https://github.com/panzhifu/folio/releases) page, one set per tagged
version. The app can also check that page for updates from inside
(Settings → About), and it does so anonymously — no account, no token.

| Platform | Package | Notes |
|---|---|---|
| Linux x86_64 | `.tar.xz` · `.deb` · `.rpm` | Needs a working Vulkan driver; Wayland and X11 both verified |
| macOS Apple Silicon | `.zip` containing `Folio.app` | macOS 11+; unsigned — allow it once under Privacy & Security on first open |
| Windows x64 | NSIS installer or portable `.zip` | Built in CI; not yet exercised on real hardware |
| Android | `arm64-v8a` `.apk` | Android 8.0 (API 26) or newer, 64-bit devices |

## Known limitations

- macOS builds are unsigned and not notarized (no developer certificate).
- The Windows package has never been run on a real Windows machine yet — treat the
  first install as a test.
- Android ships `arm64-v8a` only, and the APK does not bundle the third-party
  license texts (every desktop package does).
- No text layer in PDF / AZW4 / DjVu (see the formats table).
- The Notes screen is an explicit empty state: the annotation backend and the
  highlight write-path have landed; the interface has not.
- HarmonyOS and iOS host projects live in the tree, but are not part of the
  release matrix.

## Roadmap

Directions, not promises:

1. **Notes & annotations UI** — wire the existing backend to the screen.
2. **A text layer for PDF / AZW4 / DjVu** — selection, annotations and search
   inside fixed-layout documents.
3. **Read-aloud (TTS)** — the backend dependencies are already in place for Linux
   and Android; the engine itself is not written yet.
4. **iOS and HarmonyOS hosts**, once they enter the release matrix.

## Building from source

```bash
cargo run                      # desktop window (Linux / macOS / Windows)
cargo test --workspace --lib   # the fast, complete test loop
```

Linux needs the development packages for fontconfig, Wayland and xkbcommon, plus a
working Vulkan driver. Android cross-compilation, packaging, logging configuration
and the release process are described in the developer handbook:
**[doc/development.md](doc/development.md)** (中文).

## Under the hood

```
crates/folio-core/   the engine, zero GPUI dependency: DOM, CSS subset, layout,
                     anchors & annotations, 21 format providers, SQLite (library,
                     progress, annotations, reading stats, page tables), logging

crates/folio-app/    the shell: adaptive navigation, reader, shelf, settings,
                     stats; desktop and Android share one rendering path — only
                     the platform host layer differs
```

Two rules keep this honest:

- `folio-core` never depends on GPUI. That is what makes it fully coverable by
  `cargo test` and reusable inside the Android host.
- Anchors are **chapter-internal character offsets**, not page numbers. Page
  numbers die the moment font size or window width changes; offsets don't.

## License

MIT. The third-party license texts ship inside every desktop package
(`THIRD-PARTY-NOTICES.txt` — where each platform puts it is described in
`packaging/README.md`). The `reference/` directory — read-only upstream clones
used for design study, some of them AGPL — is gitignored and takes no part in the
build.
