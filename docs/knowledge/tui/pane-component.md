---
title: The pane slot and its scrollbar
summary: The dispatcher that owns one component per kind of window for a slot: what it delegates in which order, and the scrollbar it paints.
refs: src/tui/pane_component.rs, PaneComponent, pane_id, pane_slot
---

`src/tui/pane_component.rs` is the dispatcher for one pane slot: it owns one component of every kind, keeps the scrollbar state, and picks the live one from the window its slot holds in the stack. A slot therefore switches windows without losing the state of the ones it left, because that state lives in `AppState` keyed by the window.

An event reaches the pane in a fixed order: the window's own text input first (such as the Discussion prompt), then the scrollbar (thumb drag, track click, wheel), then the title-bar click, which exists for `FilePreview` only, then the pane's own mouse handling, and only then r3bl's keyboard dispatch. The scrollbar is drawn with half-block characters for double vertical resolution; a terminal on its alternate screen has none, because there is nothing to scroll.

`pane_id` and `pane_slot` translate between a slot number and the `FlexBoxId` r3bl routes by, and `pane_slot` answers `None` past the maximum.
