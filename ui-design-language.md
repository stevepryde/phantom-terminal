# Phantom Terminal UI Design Language

## Principles

Phantom Terminal is a compact desktop tool for repeated terminal work. Screens should feel quiet, fast, and direct: dense enough for power users, but with clear hierarchy and no decorative chrome that competes with the terminal.

## Locked Decisions

These rules capture deliberate product decisions and should not be changed casually.

| Area | Locked Rule |
| --- | --- |
| Terminal tabs | Horizontal tabs fill the full space between vertical separators. Active state is a bottom underline only: no bubble, pill, or persistent filled background. Vertical tabs use the same transparent tab surface in a left sidebar, with horizontal separators and an active right border. |
| Terminal pane inset | Terminal text has a consistent `8px` inset on all sides, while backdrop artwork fills the pane edge-to-edge. If the terminal emulator creates a single empty top screen row, visually trim that row rather than removing the intentional pane inset. |
| Settings affordance | Horizontal tab chrome exposes an icon-only Settings action at the top-right edge. Vertical tab chrome exposes the same action in a small sidebar header row so it never overlaps terminal output. |
| Font settings | Terminal font is app-wide and lives in Appearance. Shell profiles must not expose font or appearance controls unless a full appearance-profile feature is intentionally designed. |
| Profile settings | Profiles are listed first, then edited one at a time. Do not show multiple profile forms inline. |
| Default profile | Default selection happens with the icon-only star action on each profile row. Do not reintroduce a separate default-profile dropdown. |
| Autosave | Settings save immediately on change. Do not add a save button for settings panes. |
| Native dialogs | Do not use native `confirm`, `alert`, or `prompt` for app UI. Use inline confirmation, in-app dialogs, or toasts. |
| Native app control UI | The native Rust app uses egui for non-terminal controls and contextual panels. The terminal viewport stays custom-rendered by `phantom-gfx`; do not rebuild settings, sidebars, forms, sliders, or inspectors as ad-hoc terminal-renderer widgets. |

## Palette

| Token | Value | Usage |
| --- | --- | --- |
| App background | `#0b0b0e` | Main window and terminal-adjacent surfaces |
| Sidebar surface | `#111116` | Settings navigation and persistent chrome |
| Elevated surface | `#1a1a20` | Popovers and menus |
| Control fill | `rgba(255,255,255,0.05)` | Buttons, dropdowns, inactive controls |
| Control fill active | `rgba(255,255,255,0.15)` | Selected navigation and active rows |
| Divider | `rgba(255,255,255,0.10)` | Borders and separators |
| Text primary | `rgba(255,255,255,0.90)` | Primary labels and values |
| Text secondary | `rgba(255,255,255,0.55)` | Secondary row metadata |
| Text muted | `rgba(255,255,255,0.35)` | Hints and empty states |
| Focus accent | `rgba(56,189,248,0.60)` | Keyboard focus and resize affordances |
| Terminal selection | `#ffffff24` | Text selection overlay; preserve ANSI foreground colors |
| Danger | `rgba(239,68,68,0.30)` | Destructive hover states |

Rows and menu items must define hover states. Avoid single-hue pages by using neutral surfaces with restrained blue focus accents and red only for destructive actions.

## UI Themes

UI themes are separate from terminal color themes. They may tint the title bar,
tab chrome, active tab accent, and a very dim terminal backdrop wash or local
fantasy background image, but they must not rewrite ANSI foreground/background
colors or reduce terminal text contrast.

| Preset | Chrome Direction | Terminal Backdrop Rule |
| --- | --- | --- |
| Phantom | Neutral graphite | No decorative wash |
| Aurora | Cool cyan with restrained violet | Low-opacity multi-stop gradient wash |
| Ember | Warm red/orange balanced with violet | Low-opacity multi-stop gradient wash |
| Cobalt | Blue with teal and indigo accents | Low-opacity multi-stop gradient wash |
| Verdant | Green balanced with cyan and muted gold | Low-opacity multi-stop gradient wash |
| Violet | Purple with restrained magenta and indigo | Low-opacity multi-stop gradient wash |
| Amethyst | Gemlike purple with soft rose and blue | Low-opacity multi-stop gradient wash |
| Ultraviolet | Saturated violet balanced by blue | Low-opacity multi-stop gradient wash |
| Sapphire | Deep blue with cyan and indigo accents | Low-opacity multi-stop gradient wash |
| Glacier | Icy cyan-blue with pale highlight | Low-opacity multi-stop gradient wash |
| Lagoon | Teal-blue with green undertones | Low-opacity multi-stop gradient wash |
| Emerald | Rich green with teal and lime undertones | Low-opacity multi-stop gradient wash |
| Jade | Muted green-teal with a warm glint | Low-opacity multi-stop gradient wash |
| Silver | Dark metallic gray with cool highlights | Low-opacity multi-stop gradient wash |

Theme presets should remain a validated enum in Rust. Do not allow arbitrary CSS
strings, external images, or remote background URLs through config.

Terminal backdrop images live under `public/backgrounds/` and are referenced as
local assets only. The selected backdrop is a separate validated setting from
the UI color theme, surfaced directly below the UI theme dropdown in Appearance.
Keep them dark, low-detail, and low-opacity; artwork should read as embossed
texture behind text, not as an illustration competing with the terminal buffer.
Image opacity is a separate bounded setting from `0` to `60`; disable the
slider when the selected backdrop is `None`.

| Backdrop | Rule |
| --- | --- |
| Phantom | Default branded fantasy backdrop |
| Dragon | Alternate fantasy backdrop |
| None | No image; keep only the theme's dim gradient wash |

## Typography

| Role | Face | Weight | Size | Notes |
| --- | --- | --- | --- | --- |
| Section heading | System sans | 500 | `16px` | Used once at the top of a settings pane |
| Sidebar heading | System sans | 600 | `12px` | Uppercase with positive tracking |
| Field label | System sans | 400 | `12px` | Muted, above the control |
| Body | System sans | 400 | `14px` | Primary UI copy |
| Metadata | System sans | 400 | `12px` | Hints, summaries, empty states |
| Terminal text | App-wide Appearance font | 400 | User setting | Use one terminal font across profiles unless appearance profiles are explicitly added |
| Code-like inputs | UI monospace | 400 | `12px` | Commands, args, paths, color values |

Do not scale font sizes with viewport width. Letter spacing should stay at `0` except uppercase sidebar labels.

## Spacing

| Token | Value | Usage |
| --- | --- | --- |
| `space-1` | `4px` | Tight icon and row gaps |
| `space-2` | `8px` | Control padding and small gaps |
| `space-3` | `12px` | Form row gaps |
| `space-4` | `16px` | Section grouping |
| `space-6` | `24px` | Pane vertical rhythm |
| `space-8` | `32px` | Main settings inset |

Settings forms should group related fields with `12px` gaps and avoid nesting cards inside cards.

## Sizing

| Token | Value | Usage |
| --- | --- | --- |
| Control height | `32px` minimum | Inputs, dropdowns, compact buttons |
| Icon button | `28px` or larger | Toolbar and inline actions |
| Row hit target | `40px` minimum | Clickable list rows |
| Settings sidebar | `160px` to `400px` | Resizable navigation |
| Tab sidebar | `144px` to `360px` | Resizable vertical terminal tabs |
| Settings content | `672px` max | Keeps form rows readable |

Inputs, dropdowns, and adjacent buttons should share heights within a row.

## Rounding And Surfaces

| Element | Radius | Guidance |
| --- | --- | --- |
| Window | `14px`, `0` maximized | Matches transparent macOS shell |
| Linux window edge | `14px`, `0` maximized | Use a 1px Breeze-like inset outline on undecorated Linux windows so dark desktops still show a clear app edge |
| Buttons and inputs | `4px` | Compact and utilitarian |
| Menus and panels | `4px` | Use borders and background contrast |
| Cards | `4px` | Use sparingly for repeated items only |

Prefer dividers and surface contrast over card-heavy layouts. Page sections should remain unframed unless a repeated item or modal needs containment.

## Forms And Navigation

| Pattern | Rule |
| --- | --- |
| Dropdowns | Use `CustomDropdown`; never use native `<select>` |
| Profile settings | Show a profile list first; edit one profile at a time in a detail screen |
| Default profiles | Set the default from an icon-only star action on each profile row, not a separate dropdown |
| Autosave | Every setting control should call the config update path immediately |
| Destructive actions | Put delete actions in the detail/edit screen, not the list row, and use inline confirmation before deleting |
| Back navigation | Detail screens should offer a clear back action near the title |
| Editable rows | The primary row area should be clickable, with separate icon affordances for secondary actions |
| Terminal tabs | Horizontal tabs fill the full space between vertical separators; selected tabs use only a bottom underline, while inactive tabs stay transparent except for temporary hover fill. Vertical tabs live in a left sidebar, use horizontal separators, and show selection with a right-aligned border. |
| Terminal pane | Use a consistent `8px` text inset on all sides while letting the backdrop fill edge-to-edge; keep emulator blank-row trimming separate from intentional layout padding |
| Settings action | Keep the Settings action as an icon-only chrome button near the top-right of the active chrome area; route it through the same settings toggle path as the keyboard shortcut and command palette. Draw the icon with tintable chrome geometry rather than font or emoji glyphs. |
| Chrome hover | Tabs and close buttons use brief background-opacity fades on hover. Hovering a close button also keeps its parent tab hovered. Close and Settings icons fade toward the current UI theme accent. Settings hover changes icon colour only, with no hover background or scale. Tab, close, and Settings hover targets use the pointer cursor. |

## Accessibility

| Area | Rule |
| --- | --- |
| Contrast | Primary text and controls must remain readable against dark surfaces |
| Keyboard | Buttons, dropdowns, resize handles, and rows must be keyboard reachable |
| Focus | Focus states must be visible and not rely on color alone where practical |
| Hit targets | Interactive rows should be at least `40px`; compact icon buttons at least `28px` |
| Labels | Inputs and custom controls require visible labels or accessible names |

## Motion

Use minimal motion. Menu open/close can be instant; avoid animated settings transitions unless they improve orientation. Respect reduced-motion expectations by keeping essential state changes non-animated.

Chrome affordances may use short hover fades around `140ms`. Avoid springy, scaling, or decorative movement in tab chrome.

## Iconography

Use Lucide icons for actions and navigation. Icons inside buttons should have an accessible label or adjacent text. Prefer familiar action icons such as plus, edit, trash, back, terminal, and star for default selection.

## Component Library

| Component | Rule |
| --- | --- |
| Native egui layer | Use for settings, contextual side panels, command details, inspectors, forms, sliders, steppers, color controls, and future right-side panels in `phantom-app` |
| Terminal renderer | `phantom-gfx` owns terminal cells, glyph rendering, cursor, selection, and terminal backdrop only |
| `CustomDropdown` | The only dropdown/select primitive for app UI |
| `IconButton` | Use for chrome-like icon-only actions |
| Settings `Section` | Provides title, hint, and vertical rhythm |
| Settings `Field` | Provides consistent label placement |
| Profile rows | Clickable list items with aligned metadata, an edit icon, and a star action for default selection |
| Ephemeral indicator | A small status icon in the titlebar near Settings when launch mode will not restore or save tabs |
| `WindowResizeHandles` | Linux-only invisible edge handles for the decorationless Tauri window; keep them thin enough to avoid terminal scrollbar conflicts |

## Native App Egui Rules

| Pattern | Rule |
| --- | --- |
| Control plane | egui is the default UI for all non-terminal native surfaces |
| Panel placement | Prefer a wide right side panel for contextual tools and settings; overlays remain acceptable for transient command search |
| Panel navigation | Multi-section panels use a fixed vertical tab rail at the left of the panel. Keep the rail wide enough for readable labels and keep the active tab visually selected. |
| Styling | Use Phantom dark surfaces, compact spacing, low rounding, and cyan accent focus/selection states via egui `Visuals` and `Style` |
| Settings | Autosave on change through validated `AppConfig`; edit drafts may exist inside widgets, but committed config must still validate in Rust |
| Numeric controls | Use sliders, drag values, or paired steppers/text input; never rely on single-click increment-only behavior |
| Contextual panels | Model future panels as typed app state such as Settings, Search, Shell Profile, Command Details, or Task Output |
| Terminal boundary | egui panels may resize or reserve space around the terminal, but must not own terminal glyph rendering or PTY state |
