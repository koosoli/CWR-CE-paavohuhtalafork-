---
kind: frontend_style
name: Papa Bear Web UI — Minimalist Terminal-Style CSS
category: frontend_style
scope:
    - '**'
source_files:
    - mserver/MasterService/web/assets/site.css
    - mserver/MasterService/web/assets/papa-bear.js
    - mserver/MasterService/web/index.html
    - mserver/MasterService/web/browser.html
    - mserver/MasterService/web/mods.html
    - mserver/MasterService/web/browser-detail.html
    - mserver/MasterService/web/mod-detail.html
---

The only frontend styling in this repository belongs to the Papa Bear Master Service web interface, located under `mserver/MasterService/web/`. It is a small, self-contained static site served by the Rust service with no build step, framework, or asset pipeline.

**What system/approach is used**
- Plain HTML pages (`index.html`, `browser.html`, `mods.html`, `browser-detail.html`, `mod-detail.html`) linked against a single stylesheet and one vanilla JavaScript module.
- No CSS preprocessor, no component library, no design-token system, no bundler. The style sheet is written in raw CSS and loaded directly via `<link>` tags.
- The JavaScript (`assets/papa-bear.js`) is an IIFE that fetches JSON from the `/v1/*` REST endpoints and injects DOM nodes; it uses inline `style="height: …px"` for the player-history bar chart rather than CSS classes.

**Key files and packages**
- `mserver/MasterService/web/assets/site.css` — the sole stylesheet (456 lines).
- `mserver/MasterService/web/assets/papa-bear.js` — the single-page client script (~990 lines).
- `mserver/MasterService/web/{index,browser,mods,browser-detail,mod-detail}.html` — five static HTML templates, each declaring a `data-page` attribute on `<body>` to drive which JS loader runs.

**Architecture and conventions**
- **Monolithic stylesheet**: All visual rules live in one file. There are no scoped stylesheets per page and no shared partials.
- **BEM-like class naming**: Classes follow a flat `block-element` convention (e.g. `.top-menu`, `.landing-summary`, `.detail-panel`, `.history-bar`, `.status-chip`). State is expressed via additional modifier classes such as `.active`, `.is-selected`, `.status-lobby`, `.observation-self_reported`, `.observation-verified`, `.observation-unreachable`.
- **Terminal / CRT aesthetic**: The palette is hard-coded throughout the CSS — black background (`#000`), green text (`#0e9b0e`), bright-green accents (`#96ff96`), muted green subtext (`#79b979`), yellow for lobby state (`#d8d14a`), red for unreachable (`#ff7070`). Fonts are monospace (`Courier New`), borders use the same green color, and odd rows get a slightly darker background (`#030f03`).
- **Layout via CSS Grid and Flexbox**: Responsive layouts are built with `display: grid` (e.g. `.landing-summary` three-column, `.detail-columns` two-column) and `display: flex` for toolbars and menus. No CSS-in-JS or utility-first approach is used.
- **Responsive breakpoints**: Two `@media (max-width: ...)` blocks at 760px and 520px progressively collapse grids to single columns and transform tables into block-stacked cards, injecting column labels via `::before { content: "..." }` pseudo-elements.
- **No design tokens**: Colors, fonts, spacing, and border radii are repeated as literal values across the stylesheet rather than being centralized in `:root` variables or a theme object.
- **Inline dynamic styles**: The player history chart computes bar heights with inline `style="height: ${Math.max(...)}px"` inside template literals in `papa-bear.js`; there is no CSS variable-driven approach for dynamic sizing.

**Conventions and constraints**
- Every HTML page sets `lang="en"`, includes a `<meta charset="UTF-8">`, a viewport meta tag, links `site.css`, and sets a `data-page` value on `<body>` so the single JS bundle can dispatch to the correct loader function.
- All user-supplied strings are passed through an `escapeHtml()` helper before insertion into the DOM, preventing XSS in the rendered tables and detail panels.
- Navigation is generated imperatively by `renderTopMenu()`, which reads `document.body.dataset.page` to mark the active link with the `.active` class.
- The UI has no theming switcher, dark/light mode toggle, or external font imports — the look is fixed to the terminal-green palette defined in `site.css`.