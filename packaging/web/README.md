# Hosting PhotoCraft for the web

`photocraft-web-<version>.zip` (from the GitHub release, or `packaging/web/package.sh`) holds a
static site in `photocraft-web-<version>/`:

| File | What it is |
|---|---|
| `index.html` | The page. It loads everything through relative URLs. |
| `photocraft-web-<hash>.js` | wasm-bindgen glue (generated, ES module) |
| `photocraft-web-<hash>_bg.wasm` | The app: about 19 MiB raw, 8 MiB with gzip, 5.6 MiB with Brotli (see [Sizes](#sizes)) |
| `_headers`, `.htaccess` | Sample header rules for Netlify/Cloudflare Pages and Apache |

There is no server-side code. Upload the folder's contents anywhere that serves static files.

## Sizes

Measured on the 0.2.x build (`packaging/web/package.sh`; gzip `-9`, Brotli quality 11):

| File | Raw | gzip | Brotli |
|---|---|---|---|
| `photocraft-web-<hash>_bg.wasm` | 19,680,726 bytes (18.8 MiB) | 8,177,260 (7.8 MiB) | 5,868,731 (5.6 MiB) |
| `photocraft-web-<hash>.js` | about 160 KB | about 23 KB | about 20 KB |

Per-file upload limits: Cloudflare Pages and Workers static assets reject any file over
**25 MiB** (26,214,400 bytes). The wasm fits, and `packaging/web/package.sh` fails the build if
it ever grows past 24 MiB, so a release can't ship a file a common host refuses. (Releases up to
0.2.0 shipped a 25.8 MiB wasm, which Cloudflare rejected; issue #198.) The size comes from the
`wasm-release` Cargo profile (fat LTO, size-optimized code with the pixel crates kept at full
speed) plus `wasm-opt -Oz`.

## Any path works

All URLs in `index.html` are relative (`public_url = "./"` in `apps/photocraft-web/Trunk.toml`),
so the site works at a domain root (`https://example.com/`), under a prefix
(`https://example.com/tools/photocraft/`) and from a CDN bucket. The asset names carry a content
hash, so they can be cached forever. Only `index.html` needs revalidation.

## Required server settings

- **MIME type:** serve `.wasm` as `application/wasm`. Browsers refuse to stream-compile it under
  any other type, and the app then loads slowly or not at all. Serve `.js` as `text/javascript`.
  Most hosts already do both. For nginx, check that `mime.types` has `application/wasm wasm;`.
- **Compression:** turn on gzip or Brotli for `.wasm`, `.js` and `.html`. That takes the
  download from about 19 MiB to about 8 MiB (gzip) or 5.6 MiB (Brotli). You can also
  precompress (`brotli -k *.wasm`) and let the server send `Content-Encoding: br`.
- **Caching:** `Cache-Control: public, max-age=31536000, immutable` on the hashed `.wasm` and
  `.js` files, and `no-cache` on `index.html`.
- **HTTPS:** WebGPU (and the clipboard) only work in a secure context, which means `https://`
  or `http://localhost`. Over plain HTTP elsewhere, the app falls back to WebGL2.
- **No special isolation headers:** PhotoCraft doesn't use `SharedArrayBuffer`, so it doesn't
  need `Cross-Origin-Opener-Policy` or `Cross-Origin-Embedder-Policy`. If your site already sends
  COEP `require-corp`, also send `Cross-Origin-Resource-Policy: same-origin` (or `cross-origin`
  when the files live on a CDN) on the app's files.

nginx example:

```nginx
location /photocraft/ {
    types { application/wasm wasm; text/javascript js; text/html html; }
    gzip on;
    gzip_types application/wasm text/javascript text/html;
    location ~* \.(wasm|js)$ { add_header Cache-Control "public, max-age=31536000, immutable"; }
    location ~* index\.html$ { add_header Cache-Control "no-cache"; }
}
```

Local test: `python3 -m http.server 8765` inside the folder, then open http://localhost:8765/.

## Embedding in a page (iframe)

```html
<iframe
  src="https://example.com/photocraft/"
  title="PhotoCraft image editor"
  style="width: 100%; height: 720px; border: 0;"
  allow="fullscreen; clipboard-read; clipboard-write"
  allowfullscreen>
</iframe>
```

- The app fills the iframe and follows its size, so size the iframe and not the app.
- Keyboard shortcuts go to the iframe after the user clicks into it, as with any embedded app.
- **Cross-origin embeds** work. Preferences are kept in the iframe's `localStorage`. Browsers
  that partition or block third-party storage may forget them between visits, and the app
  then starts with defaults.
- **Sandboxed iframes** need at least
  `sandbox="allow-scripts allow-same-origin allow-downloads allow-popups"`. Without
  `allow-same-origin` there's no storage. Without `allow-downloads`, Save and Export (browser
  downloads) are blocked.
- Don't send `X-Frame-Options: DENY` or a `frame-ancestors` CSP that excludes the embedding page.

## Renderer selection and fallback flags

PhotoCraft renders with wgpu. It uses **WebGPU** when the browser has it and falls back to
**WebGL2** on its own. URL query flags override this, and they work on the iframe `src` too:

| Flag | Effect |
|---|---|
| *(none)* | WebGPU if available, otherwise WebGL2 |
| `?webgl` | Force the WebGL2 backend (useful when a WebGPU driver misbehaves) |
| `?cpu` | Force the CPU canvas path (slowest, most compatible) |

For example: `<iframe src="https://example.com/photocraft/?webgl" ...>`.

A browser with neither WebGPU nor WebGL2 gets a message in place of the app.
