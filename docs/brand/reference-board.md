# Flowmux Reference Board

This board is for visual direction, not copying. The goal is to identify why
adjacent open-source brands work, then constrain the next Flowmux logo pass.

## Direct Neighbors

| Project | Reference | What works | What Flowmux should not copy |
| --- | --- | --- | --- |
| Herdr | https://github.com/ogulcancelik/herdr | One reduced idea: animal silhouette plus terminal prompt. Works as a tiny mark because it is mostly one shape. | Animal head, prompt-as-face construction, grayscale sheep/ram identity. |
| amux | https://github.com/mixpeek/amux | Strong GitHub header system: neon A mark, product phrase, terminal mockup, command examples. It sells the product quickly. | Neon triangle/A shape, control-plane positioning, dense landing-page badge treatment. |
| dmux | https://github.com/standardagents/dmux | Distinctive striped orange wordmark. It feels terminal/scanline without drawing a terminal window. | Orange stripe style, lowercase segmented letterforms, wide display-wordmark-first approach. |

## Adjacent Terminal Brands

| Project | Reference | Useful lesson |
| --- | --- | --- |
| Zellij | https://zellij.dev/ | A terminal mark can be colorful and friendly while staying geometric. Its mosaic/tile metaphor fits the name and product. |
| Ghostty | https://ghostty.org/ | A mascot can work when the name gives permission. Simple silhouette plus terminal face beats abstract tech shapes. |
| WezTerm | https://wezterm.org/ | Very simple terminal/app icon logic: dark rounded square, bold glyphs, high contrast. |
| tmux | https://github.com/tmux/tmux | No strong logo dependency. The project is known by name and utility, a reminder that Flowmux does not need an overdesigned mark. |

## Open-Source Brand Systems

| Project | Reference | Useful lesson |
| --- | --- | --- |
| CNCF | https://www.cncf.io/brand-guidelines/ | Treat the logo as a system: primary logo, secondary logo, color variants, spacing, scaling rules. |
| Node.js | https://nodejs.org/en/about/branding | Separate mascot, icon, horizontal logo, stacked logo, and black/white variants. One asset should not do everything. |
| Rust | https://rustfoundation.org/policy/rust-trademark-policy/ | Keep source assets and reuse rules clear. Even small projects benefit from explicit logo files and usage notes. |
| Mozilla redesign | https://www.wired.com/2016/08/mozilla-wants-help-redesign-logo-seriously/ | Share constrained directions and refine with critique instead of asking for arbitrary submissions. |

## Patterns Worth Borrowing

- One memorable metaphor, not a collage of features.
- Strong silhouette first; color and detail second.
- A mark that works in monochrome at 16px, 32px, and 64px.
- Separate deliverables: app mark, horizontal logo, GitHub/social header.
- README header can be richer than the logo itself.
- Terminal identity can be implied through rhythm, scanlines, prompt glyphs, or panes; it does not need a literal terminal window.

## Patterns To Avoid

- Generic node graphs, orbit lines, and "AI network" symbols.
- Pane grids with decorative gradient paths over them.
- Logo marks that look like product UI icons.
- Too many colors in the core mark.
- Forcing `F`, `flow`, `mux`, terminal, agents, tmux, and worktrees into one drawing.
- A wordmark that depends on a local font being installed.

## Flowmux Direction

Recommended direction: **switchyard monogram**.

Flowmux should own the idea of routing many agent sessions through one
terminal-native control surface. The mark should be a reduced symbol, built
from 2-3 thick strokes, that can read as:

- a lowercase `f`;
- a switch or forked rail;
- terminal scanlines or panes only by implication.

Do not draw agents. Do not draw a full terminal window. Do not use node graph
circles as the main motif.

## Next Design Pass

1. Create 20 tiny black-only sketches as SVG thumbnails.
2. Use only thick strokes, solid fills, and negative space.
3. Test every sketch at 16px, 32px, and 64px before adding color.
4. Keep 3 finalists:
   - switchyard monogram;
   - striped/scanline wordmark;
   - optional steed/harness mascot direction, only if it stays iconic.
5. Build one GitHub/social header separately after the mark is chosen.

## Candidate Palette

Start monochrome. If the shape survives, add one accent:

- Ink: `#0b1020`
- Paper: `#f8fafc`
- Electric cyan: `#12d6df`
- Signal green: `#5ee787`
- Optional warning accent: `#ffb000`

Avoid blue-purple gradient dominance. Flowmux should feel terminal-native,
sharp, and operational, not SaaS-generic.
