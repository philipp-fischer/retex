# Static deployment

`dist/workspace/` is a complete static website after the
[browser build](BUILDING.md). The Rust engine runs as WebAssembly in a worker;
hosting does not require Rust, Python, a TeX installation or a compilation server.

The UI, workers and resource loaders use relative URLs, so the same artifact can
be mounted at `/` or at a project path such as `/retex/`. Deploy the whole directory,
including `generated/`, the worker modules and license notices. Do not deploy only
`dist/ui/` or the source `web/` directory.

## GitHub Pages

Once a public repository and site location have been selected:

1. Build and validate `dist/workspace/`, including the browser smoke test.
2. In repository **Settings → Pages**, select **GitHub Actions** as the source.
3. Have the build workflow upload `dist/workspace/` with
   `actions/upload-pages-artifact`; a deployment job uses `actions/deploy-pages`.
4. Give the deployment job `pages: write` and `id-token: write` permissions and
   the `github-pages` environment. Connect it to the successful build job.
5. Check the deployed project URL, editing, package loading and PDF download.

See GitHub's [custom workflow instructions](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)
for the current action versions and configuration. No public demo URL or automatic
deployment is assumed by this repository. The packaging script also writes
`.nojekyll` for hosts that support branch-based static publishing.

GitHub Pages has a [1 GB published-site limit](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits).
Keep generated binaries in deployment/release artifacts rather than committing
them to the source tree. Evaluate download size and hosting bandwidth before
expanding the default package collection.

## Resources, privacy and caching

The base bundle contains the initialized LaTeX format and common resources.
Additional prepublished TeX Live packs are downloaded from the same static site
and verified against their manifest. The browser keeps document contents locally;
it fetches application assets and prepared package packs from the host. Additional
packages are downloaded directly from the current TeX Live repository on
`mirrors.huaweicloud.com` (with `repo.huaweicloud.com` as a fallback). No CORS proxy,
compilation server, or browser setting changes are required. A custom Content
Security Policy must allow HTTPS connections to these mirror hosts.

Use HTTPS in production. Cache Storage and Web Crypto verification are available
in secure contexts, including localhost. Blocked persistent storage still permits
worker-session resource loading. This is not a complete offline-installable PWA:
package caching alone does not guarantee that a new browser visit works offline.

The top-bar **Storage** gauge opens package-cache settings. The default limit is
1 GiB, shared by cached package resources across projects and resource versions.
Users can inspect cached package names and sizes, remove individual package packs
with ×, change the limit, clear the cache, or request persistent browser storage.
Least recently used resources are evicted when the limit is reached; they can be
downloaded again. The gauge measures the managed package cache, with the browser's
estimated total website usage and allowance shown separately in settings.

On browsers with a writable directory picker (such as Chrome and Edge), users can
move the cache into a dedicated `ReTeX package cache` subfolder of a chosen folder.
Existing entries are copied before switching storage, and the folder choice is
saved locally. Access may need to be granted again on later visits. Firefox uses
browser-managed storage. Project files and loaded engine resources are independent
of the package cache; clearing it does not delete a project or unload its preview.

Static hosting does not need the optional `/texlive/resolve` service. The browser
reads the full TeX Live package database and resolves additional runtime files,
including dependencies, itself. Catalog metadata and checksum-verified archives
share the managed package cache. Packages absent from TeX Live still require
project files. The bundled kernel remains pinned, so downloading a package does
not guarantee compatibility with its required LaTeX version. There is no external
BibTeX/Biber or shell-command service in the static app.

## Local servers

Serve the exact static artifact:

```sh
python3 -m http.server --bind 127.0.0.1 --directory dist/workspace 8000
```

For development with an additional package resolver:

```sh
python3 tools/serve_browser.py --port 8000
```

The latter downloads checksum-verified packages from frozen TeX Live archives.
Use `--offline` to restrict it to cached archives. It supplies resources only;
typesetting continues to run in the browser.
