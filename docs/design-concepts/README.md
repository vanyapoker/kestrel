# Kestrel design alternatives

Four image-generated proposals based on the existing overview and columns screenshots and the active Omarchy palette. These are visual concepts, not implemented screens. Generated with the built-in image-generation tool; full prompts are saved in [prompts.json](prompts.json).

## Current design review

The existing interface gives files most of the screen, supports dense keyboard navigation, and already maps colors to the active Omarchy theme. Keep those strengths.

- Consolidate shared search and view controls in split mode. Keep each pane's path and navigation local so its destination stays clear.
- Replace the full active-pane accent border with a small focus marker. Keep active and inactive selections distinguishable.
- Improve secondary-text contrast and make file names slightly easier to scan. Offer compact density rather than making every row permanently larger.
- Make preview metadata a compact aligned block. Long paths can truncate in the default view and reveal their full text on hover or copy.
- Keep speed, ETA, progress, and cancel together in one quiet operation strip. Its idle state should return space to folder and selection details.
- Use one consistent family of small line icons, with theme-derived folder and file-type colors used sparingly.

## 1. Refined Classic

[Open image](01-refined-classic.png)

A labeled sidebar, Details table, and compact right inspector. Tabs use an underline; selected files use a restrained fill. The closest evolution of today's app and the most discoverable choice. It retains more permanent navigation than the other proposals.

## 2. Focus Rail

[Open image](02-focus-rail.png)

A narrow icon rail replaces the labeled sidebar, giving the file list more width. Breadcrumbs and search share one toolbar; the inspector is optional. The strongest minimal direction. Keep tooltips and allow the rail to expand to labeled places and bookmarks so navigation remains discoverable.

## 3. Twin Pane

[Open image](03-twin-pane.png)

Two balanced file tables with independent paths, shared view controls, and an optional preview. Useful for comparing folders and transferring files. It gives up the always-visible inspector to preserve space for both destinations. Existing tabs and independent pane state should remain supported.

## 4. Column Studio

[Open image](04-column-studio.png)

Three Miller columns show the path through a project, with the current directory wider and a compact image inspector. Good for nested projects and browsing with context. At narrow widths, columns must scroll horizontally and the inspector should fold away.

## Theme contract

All proposals use the same current dark theme: background `#060B1E`, panel `#040816`, text `#ffcead`, accent `#7d82d9`, selection `#252e56`, and folder yellow `#E9BB4F`. These are examples of semantic roles, not fixed application brand colors. Implementation should continue using the existing palette mappings in `src/theme.rs`, including their light-theme behavior and live reload.

Generated imagery can approximate colors and typography. Exact token values, icon consistency, readable density, and narrow-window behavior would be enforced during implementation.

Recommended direction: Focus Rail for the minimal default, with an expandable sidebar and the existing split and columns modes retained. Refined Classic is the conservative alternative if permanently visible navigation labels matter more.
