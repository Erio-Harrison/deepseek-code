# Vendored libraries

Browser-ready bundles copied unmodified from the official npm packages (no
CDN, no bundler step — the webview's CSP only allows the extension's own
nonce'd `<script>` tags, so third-party code has to ship as a local file).

| File | Package | Version | Source within the npm tarball |
|---|---|---|---|
| `marked.js` | [`marked`](https://www.npmjs.com/package/marked) | 18.0.14 | `lib/marked.umd.js` |
| `purify.min.js` | [`dompurify`](https://www.npmjs.com/package/dompurify) | 3.4.16 | `dist/purify.min.js` |

Each `.LICENSE.txt` is that package's own `LICENSE` file, copied alongside it.
`marked` is MIT; `dompurify` is Apache-2.0 (dual-licensed with MPL-2.0 per its
file header — the bundled `LICENSE` file itself is the Apache-2.0 text).

## Updating

```bash
npm pack marked dompurify
tar xzf marked-<version>.tgz -O package/lib/marked.umd.js > marked.js
tar xzf dompurify-<version>.tgz -O package/dist/purify.min.js > purify.min.js
```

Bump the version numbers in the table above to match.
