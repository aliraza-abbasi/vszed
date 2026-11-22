# GPUI list: off-screen focused item in the bottom padding (upstream candidate)

Branch `fix/gpui-list-offscreen-focused-item`. A fix to upstream GPUI, not a fork feature.

`List` renders a focused item that is scrolled out of view, so it keeps receiving keyboard input
(`layout_items`: "If none of the visible items are focused, check if an off-screen item is
focused and include it to be rendered after the visible items"). `prepaint_items` placed it right
after the last visible item. At the end of the scroll that spot is inside the list's bottom
padding, which is within the list's content mask, so the item was visible there.

Seen in vszed's Claude Code style, whose transcript list has a large bottom padding so the last
entry scrolls clear of the floating prompt box: clicking a thinking block or an Edit row focuses
it, and once it scrolled out of view it showed again under the last message (behind the prompt
box). Restarting cleared it because focus reset.

Fix: `ItemLayout::rendered_for_focus` marks that item, and prepaint places it at
`max(next origin, bounds.bottom())`, where the content mask hides it. Autoscroll math uses the
same origin, so keyboard input in it still scrolls it into view. Test:
`test_offscreen_focused_item_stays_out_of_bottom_padding` (fails without the fix: the item lands at
y = 100 inside 100px of padding).

Upstream: changes to `gpui` need a `[GPUI]` release note bullet. Nothing has been posted. Ali
decides when.
