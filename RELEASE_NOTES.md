# v0.33.1-fork.4

Two additions, both new since fork.3: the career Grand Live skip lever, and a scrolling screenshot
button. Neither does anything until you ask.

## Skip the career Grand Live (`skip_grand_live`, off by default)

The measured reason: one career session stood on the Grand Live screen eleven times, 468 s in total,
and the concert's own skip crossed zero times. The time went on a click the game was ready to take
and nobody made.

With the option on, the lever waits for the live's own controller to exist and for that controller to
say its button is up (`_isGrandLiveTop`, `_currentPhase` a `PhaseGrandLiveAnimation`, `_canSkipLive`
and `_isShowedLiveButton` all true), and then clicks its own `OnLiveButtonClicked`, once. It asks
where the game is asking. The client's own saved skip setting is never written, because that one is
yours to leave alone.

The honest state: two played runs got the click to land (8 of 13 live visits, then 4 of 6), but 2 of
those 4 pressed visits still never left the live phase and ended on a menu soft reset. This build
adds what came out of that reading: a clock that times how long the screen stands, counted doors,
and a park line. It has not been played yet, so the log of your first career with it on is the real
answer to whether it is fixed.

## Scrolling screenshot (a menu button, not an option)

The menu has a new button under Close menu. It closes the menu, scrolls the screen with real mouse
wheel notches, captures frames from the frames the game itself presents, stitches the scrolled
parts into one tall image, and fixes the parts of the screen that do not move (title bars, button
bars) in once. When it ends it writes `scrollshot_<timestamp>.png` to the game root and puts the
same image on the clipboard.

Press the menu key again during a run to stop early; what was stitched so far is still saved. It
ends by itself when three steps measure no movement (bottom of the list), after 60 steps, or at a
64 MB canvas, and tells you which.

The v1 limits: chrome that animates (blinking highlights) can leak repeats, lists that never settle
in the same place twice degrade the matching, the scrollbar column is excluded from matching but can
still show up inside the image, and horizontal scroll is out of scope.

Status: the stitching math is held by unit tests (exact offset recovery, refusing coincidental
matches, chrome once, clipboard layout). The capture, wheel and clipboard paths compile and have
never run in a live game session until yours, so on first contact: if it captures black or scrolls
the wrong window, that is new, and the log line and what screen you were on will fix it.

## Install

Close the game, replace `cri_mana_vpx.dll` in the game root, keep the original as a backup. The menu
key (`Insert` by default) opens the menu; the Config Editor's Performance tab carries
`skip_grand_live`, off. Everything here is client side presentation. No server call, purchase path,
race simulation or save format is touched.
