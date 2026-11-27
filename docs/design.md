# Design standard

Every Deskway surface uses the same visual language: the browser client (`web/static`) and the
host's panel, setup window and in-session bar (`host/src/ui`). The shared components and their
style tokens live in `web/static/design.css`. The host embeds that file at build time.
`tests/unit/design.test.mjs` checks that page and component styles use those tokens.

## Tokens (`design.css`, `:root`)

Component and page styles use only the semantic names below. No colour, size, radius, spacing
or shadow is written as a literal anywhere else.

| Group | Tokens |
|---|---|
| Colour, page | `--ground` page, `--surface` cards and dialogs, `--frame` behind pictures, `--screen` an empty picture |
| Colour, text | `--ink` text, `--quiet` secondary text, `--faint` disabled and offline |
| Colour, lines | `--line` dividers, `--line-strong` control borders, `--hover` hover and pressed fill, `--scrim` behind dialogs |
| Colour, meaning | `--accent` (+ `--accent-soft` fill, `--accent-line` border), `--ok` (+ `--ok-soft`), `--warn` (+ `--warn-soft`), `--danger` (+ `--danger-soft`, `--danger-line`), `--on-accent` text on filled colours, `--switch-off` |
| Colour, over the remote picture | `--dark` panels, `--dark-solid` the keyboard, `--dark-ink`, `--dark-quiet`, `--dark-key`, `--dark-key-quiet`, `--dark-key-down`, `--dark-hover` |
| Type | `--fs-caption` 12, `--fs-small` 13, `--fs-body` 14, `--fs-subtitle` 16, `--fs-title` 20, `--fs-display` 28; `--lh-body` 1.6, `--lh-tight` 1.4, `--lh-title` 1.25; weights 300 (text) and 400 (emphasis, buttons) |
| Spacing | `--s1` 4, `--s2` 8, `--s3` 12, `--s4` 16, `--s5` 20, `--s6` 24, `--s8` 32, `--s12` 48 |
| Radius | `--r-control` 4 (buttons, inputs, switches, tags), `--r-box` 6 (cards, dialogs, menus, notices, panels); circles use `50%` |
| Height | `--h-control` 44 (actions and inputs, touch-sized), `--h-compact` 32 (in-row actions, selects, the toolbar with a mouse) |
| Shadow | `--shadow-card`, `--shadow-card-hover`, `--shadow-pop` (menus, dialogs, the toolbar), `--shadow-float` (anything over the remote picture) |

There is one font stack per language (`--font-zh-hans`, `--font-zh-hant`, `--font-en`), selected
by the document's `lang` attribute, and `--font-mono` for codes.

## Deskway mark

`host/src/icon.rs` draws the shared D formed by a screen and a circle. The browser favicon,
standard manifest icons and README use the three-colour mark on a transparent background.
The Windows app and tray icons and the macOS app icon use the white mark on a rounded blue
gradient tile. The macOS menu bar uses a monochrome template with one opacity per region.
The maskable and Apple touch icons use the gradient to the edges for system cropping.

## Components (`design.css`)

Each component is defined once, and both apps use the same classes.

| Component | Classes and rules |
|---|---|
| Button | `.btn` is a secondary (outlined) button by default. `.primary` is filled with the accent colour, `.text` has no border or fill, and `.danger` uses the danger colour for its text, or as its fill when combined with `.primary`. The height is `--h-control`, or `--h-compact` with `.compact`. `.icon` is a square button holding only an icon, framed only while hovered or open (for example a window's close button). `.more` is the borderless, unfilled ⋯ menu button. Its dots align with the content's right edge and use `--quiet` at rest, `--ink` on hover and `--accent` while pressed or open. A transparent 40 × 44 px hit area takes no layout space, and the keyboard focus outline remains. A view or dialog has at most one primary button, placed rightmost among its actions. |
| Input | `.input` uses `--h-control` and `--r-control`. Native `<select>` is not used: a choice among a few options is a menu (`.dropdown`) whose entries are `menuitemradio`, with the current choice checked. |
| Form fields | `.fields` is a column of labelled inputs. `.check` is a checkbox with its label, and `.error` (`role="alert"`) is the line below the fields that says what went wrong. |
| Switch | `.switch[aria-checked]`. |
| Dialog | `.overlay` (the scrim) contains `.dialog`, which has a title (`h2`), a body (`p`) and right-aligned `.dialog-actions` with the primary action rightmost. Every confirmation that does not concern a single row is a dialog. All dialogs, including forms in a dialog, are made in one place (`js/dialogs.js`). |
| Inline confirm | `.confirm` is used only to confirm, in place, the removal of one row of a list. |
| Notice | `.notice` is one line of state on a page: a `.dot`, the text and an optional `.btn.compact`. Over the remote picture the same component is `.notice.dark`. |
| Menu | `.dropdown` contains the button that opens it and a `.menu` with `.menu-head`, `.menu-item` (`.danger` for removal) and `.menu-sep`. The menu drops down from its button, aligned to the button's right edge. Items are sized to their text (at least 40 high, wrapping as needed) and have no decorative icons. All menus are made in one place (`js/views/menu.js`), and a menu stays open when the page is redrawn. An item's own actions are always in a menu under its ⋯, never laid out as buttons next to its name. |
| Status | `.status` with a `.dot` (`.ok`, `.warn`, or neither for off). |
| Tag | `.tag` (`.accent`, `.ok`, or plain). |
| Spinner, loading | `.spinner` is the spinner. `.loading` is a spinner together with a description of what is being waited for. |

## Page structure

Every page has the same shape: a top bar, then the page title, then the content, aligned to the
top on the page ground. Content is not wrapped in a card; a card is used only for one item of a
list, such as a computer.

The browser's top bar (`.topbar`) shows `Deskway` on the left and, on the right, the language
menu and, once signed in, the account. The title (`.heading h1`, `--fs-display`) is in the same
place on every page. Forms are at most 360 wide and pages at most 1280.

On the browser's sub-pages (a computer, **My devices**), the title begins with a back control: a
`‹` drawn in the title's own typeface (`.title-back`, sized in `em`, on the baseline). Its
full-size touch target extends into the margin and never covers the title. Only the mark
navigates back; the title itself remains plain text. Anything that belongs to the title sits on
the same row (a count after it, a ⋯ at the far end). A line below the title, such as a status,
starts where the title row starts and wraps without shifting anything on the row.

The client behaves like an app rather than a web page: nothing on its pages can be selected,
highlighted on tap, dragged or given the browser's callout menu. Only input fields accept text.

The host panel shows the computer's name and status at the top left and the language menu and
the window's close button at the top right. Below them come the settings, then the devices, and
signing out is at the bottom.

The language is chosen with a single component, the language menu: a globe, the current
language's name in that language, and a menu (`.lang-btn`). It is the same in the browser and in
the host's windows, where it comes from `host/src/ui/lang.js`. It appears only on top-level
pages, always at the top right. Sub-pages, dialogs and error pages do not show it.

## Words

Every element that holds text must accept text of any length: nothing that holds words has a
fixed height, wrapping is allowed, and every view is checked in Simplified Chinese, Traditional
Chinese and English.

The exception is a title in a bar or a section heading, which stays on one line. A name too long
to fit ends in …, while a count after it (· 12) is always shown in full. Titles should be written
short enough not to need truncation.

Each thing has one name across both apps, as listed in the glossary in `docs/i18n.md`.

## Exceptions

Exceptions are allowed only where `tests/unit/design.test.mjs` lists them, each with its reason:
the on-screen keyboard's key sizes (which follow a real keyboard's proportions), the remote
pointer, and the drawn icon of the clipboard ball.
