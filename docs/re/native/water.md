# Water and its fake reflections (native renderer)

China's lake and Arizona's river show what stands on their banks reflected in the water. The
reflections are geometry in the track data, drawn by the game's back-to-front polygon order:

* **Water**: the `lag` family (`a)201lag` .. on China, 9 statics; `a)601lag` .. on Arizona,
  48), opaque textured polygons. The surfaces are not level (China's pieces slope by up to
  500 units under one boat group).
* **Reflection cards**: flat polygons painted with the reflected picture, hanging under the
  water: China `areflex` (4, one under each group of boats, about 6 500 units tall),
  Arizona `a(051lk` .. `a(058lk` (46, in pairs one under the other, under the canyon's wall
  cards `a(031lk` .. `a(038lk`, in the same plane). Arizona's cards carry a sort bias of
  -500 (model +C4h, `world.md`), its water +6 000.
* **Objects standing in the water**: flat ones whose foot is under the water (Arizona's wall
  cards reach about 80 units below the river; China's tree screen `a)012for` and the reeds
  `a1palud` .. `a3palud`).

Without a depth buffer the game draws the water, then the cards and the standing objects'
feet over it. With a depth buffer (the native renderer, and the Glide path with
`depth_buffer = on`) the water in front hides them: no reflections, and a band of bare water
at the foot of Arizona's walls. Seen on the Glide path with `SR_DEPTH=0` (the 1997 order) and
in a video of the original: the reflections are there.

## The native renderer (`native/water.rs`)

At the race's scene build the water statics' materials become `Layer::Water` (drawn as the
world; their pixels get the stencil class `ST_WATER`), the cards' `Layer::Reflection`, and a
standing object keeps its faces and gets a copy of them as `Layer::Reflection`.
`Layer::Reflection` is drawn after the decals, only over pixels whose front-most surface is
water, whatever the depth, writing no depth: anything in front of that water (a bank, a boat)
already hides it, as the game's order does.

Classification, from the data: water = family `lag` (the flags `a1flag` are family `flag`);
among the other statics, those whose faces lie in one plane (within 10 degrees) with water
under the corners and middle of their top edge (the water's height there from its
triangles): a card hangs at least 1 000 below that water and reaches at most 1 000 above it,
a standing object reaches below it otherwise. Result: China 9 water, 4 cards, 12 standing;
Arizona 48 water, 46 cards, 23 standing; the other tracks have no water (Canada's frames
byte-identical to the build before).

Checked: China's lake before the finish (frames 4 556-4 604 of the documented race) and
Arizona's river (frames 1 700-2 200), side by side with the Glide path in the game's order
(`SR_DEPTH=0`): the same reflections and wall feet.
