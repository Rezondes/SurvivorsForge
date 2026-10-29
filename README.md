# SurvivorsForge homepage

The static homepage, published with GitHub Pages from this branch (`gh-pages`). The code of the
launcher and the platform lives on `main`.

## Publish

On GitHub: *Settings → Pages → Build and deployment*, Source *Deploy from a branch*, branch
`gh-pages`, folder `/ (root)`. The page then appears at `https://rezondes.github.io/SurvivorsForge/`.
`.nojekyll` makes Pages serve the files as they are.

## Links to the platform

The links to the platform website point at the official instance, `https://forge.rezondes.net`.

## Files

| File | What |
|---|---|
| `index.html` | The page |
| `style.css` | The launcher's look: colours, fonts, buttons, headings |
| `assets/icon.png` | App icon, also the favicon |
| `assets/fonts/` | Uncial Antiqua and Marcellus with their SIL Open Font Licenses |

Try it locally: `python -m http.server` in this folder, then http://localhost:8000.
