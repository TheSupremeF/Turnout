# Turnout Design QA

- Reference: `reference-relay-routing.png` (1487 × 1058)
- Implementation capture: `qa-implementation.png` (1487 × 1058)
- Side-by-side artifact: `design-qa-comparison.png`
- Route under test: `http://127.0.0.1:4173/`

## Pass 1

- Layout: the initial implementation used a 208 px sidebar and placed the provider strip and workspace 12–15 px too high. The right inspector was also 13 px too narrow.
- Typography: matrix labels, route metadata, file paths, and inspector copy were visibly denser and smaller than the reference.
- Vertical rhythm: the route header and table header were 4 px shorter in aggregate, shifting the selected row upward.
- Browser: connection test, apply state, command palette, and responsive overflow worked. A missing favicon caused one console 404.

Fixes applied: sidebar changed to 216 px; content, provider strip, matrix, and 333 px inspector aligned to the reference; route and inspector type scales increased; table header rhythm corrected; connection result expanded; favicon request removed.

## Pass 2

- Fidelity: primary frame, top bar, navigation, title, provider strip, matrix, selected row, inspector, status colors, borders, radii, and bottom alignment now match the reference closely.
- Intentional product differences: Anthropic shows four selectable model families and the selected Claude model uses the newer version label requested by the user. Provider glyphs use the Phosphor icon family rather than copied brand artwork.
- Functionality: provider/model selection, CLI and VS Code toggles, row selection, reset, connection-test loading/success, apply loading/toast, sidebar navigation, responsive filter, command palette, Cmd/Ctrl+K, and Escape close are implemented.
- Responsiveness: tested at 1487, 1180, 980, and 720 px. The page itself does not overflow; the routing matrix becomes locally scrollable at narrow widths.
- Accessibility: semantic controls and labels are present, keyboard dismissal works, live status regions are used, focus styles remain visible, and reduced-motion preferences are respected.
- Runtime: no console or page errors during the interaction scenario.
- Build: `npm run build` passed.
- Packaging: `npm run test:sites` passed (4/4).

final result: passed
