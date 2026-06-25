# niri-media-keys

The original real-world case that motivated the eval harness. The diff adds
**media playback** keybindings only (play-pause, next, prev, and Shift+next/prev
to cycle the selected MPRIS player). There is no volume binding anywhere in the
diff - the surrounding context lines are *brightness* keys.

## Should
- Describe media / playback / MPRIS keybindings.
- Stay scoped to `niri` (or `niri/binds`).

## Should NOT
- Mention **volume** - there is no volume binding in the diff. This was the
  original hallucination: the model pattern-matched `XF86Audio*` to the common
  "media + volume" block and invented volume controls.
- Mention brightness (those lines are unchanged context).
