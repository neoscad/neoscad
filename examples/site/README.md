# Models behind neoscad.org's images

| Model | Image on the site | Made with |
|---|---|---|
| `hose_adapter.scad` | `assets/snapshot-adapter.png` | `neoscad snapshot examples/site/hose_adapter.scad --dims --size 1280x1280` |
| `phone_stand.scad` | `assets/snapshot-issues.png` | `neoscad snapshot examples/site/phone_stand.scad --issues --size 1600x1600` |

`phone_stand.scad` has three printing problems on purpose (a lip thinner
than the nozzle, an unsupported shelf, a clip floating above the bed), so
`neoscad check` and `snapshot --issues` have something to show.
The hero gearbox is `apple/Icon/hero.scad` (`scripts/apple/build-hero.sh`).
