# Kil landing page (`website/`)

Zero-dependency static landing page for Kil (SEO + GEO). One HTML file with
inline CSS, no JavaScript, no build step — any static host can serve it.

- `index.html` — the page (meta, Open Graph, JSON-LD, content)
- `robots.txt` — allows all crawlers incl. AI bots, points at the sitemap
- `sitemap.xml` — single URL, update `<lastmod>` when the page changes

All copy facts come from the repo root `README.md`. If the README changes
(version, size numbers, features), update `index.html` to match — including
the JSON-LD blocks, which must stay consistent with the visible FAQ.

Preview locally:

```sh
cd website
python3 -m http.server 8080
# open http://localhost:8080
```

## Enabling GitHub Pages

GitHub Pages (Deploy from branch) only publishes from the repo root or from
`/docs` — it cannot publish from `/website`. So pick one of these:

### Option A (not recommended): move the content into `docs/`

Copy `website/*` to the repo root `docs/` and set Pages to Deploy from
branch → `main` → `/docs`.

Risks:

- `docs/` already holds repo assets (`screenshot.png`, `icon.png`,
  `size-*.svg`) referenced by the README. Mixing site files with repo
  assets makes both harder to manage.
- Every landing-page edit touches the same directory as release-facing
  docs, so a bad edit can break README images.
- The site URL becomes `https://timwuhaotian.github.io/kil/` with
  `docs/` internals exposed alongside it.

If you still want this:

```sh
# from the repo root
cp website/index.html website/robots.txt website/sitemap.xml docs/
# then: Settings → Pages → Deploy from branch → main → /docs
```

### Option B (recommended): publish via a `gh-pages` branch

Keep `website/` as the source on `main`, and publish only its contents to
an orphan `gh-pages` branch. The site stays isolated from the repo, and
`main` history stays clean.

Repeatable flow (run from repo root on `main` whenever the page changes; the first run creates the branch):

```sh
# 1. copy website/ to a temp dir
rm -rf /tmp/kil-pages && cp -r website /tmp/kil-pages

# 2. switch to (or create) the publishing branch
git checkout gh-pages 2>/dev/null || git checkout --orphan gh-pages

# 3. replace branch contents with the site files at root
git rm -rf . >/dev/null 2>&1 || true
cp /tmp/kil-pages/index.html /tmp/kil-pages/robots.txt /tmp/kil-pages/sitemap.xml .
touch .nojekyll   # serve files as-is, skip Jekyll processing

# 4. publish
git add -A
git commit -m "Publish landing page"
git push -u origin gh-pages

# 5. back to main
git checkout main
```

Then enable Pages once: Settings → Pages → Deploy from branch →
`gh-pages` → `/ (root)`. The site goes live at
`https://timwuhaotian.github.io/kil/`.

Alternative: serve `website/` from any static host (Cloudflare Pages,
Netlify, plain nginx) — point the web root at the `website/` directory.
If the final URL is not `timwuhaotian.github.io/kil`, update the
`Sitemap:` line in `robots.txt` and the `<loc>` in `sitemap.xml`.
