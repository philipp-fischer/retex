# Static deployment

`dist/workspace/` is a complete static website after the
[browser build](BUILDING.md). The Rust engine runs as WebAssembly in a worker;
hosting does not require Rust, Python, a TeX installation or a compilation server.

The UI, workers and resource loaders use relative URLs, so the same artifact can
be mounted at `/` or at a project path such as `/retex/`. Deploy the whole directory,
including `generated/`, the worker modules and license notices. Do not deploy only
`dist/ui/` or the source `web/` directory.

## GitHub Pages

The [Pages workflow](../.github/workflows/pages.yml) builds and deploys the app on
pushes to `main`, or when run manually from the Actions tab. To enable it:

1. In repository **Settings → Pages**, select **GitHub Actions** as the source.
   No suggested template, custom domain or additional secrets are required.
2. Push to `main`, or open **Actions → Deploy browser demo → Run workflow** and
   select `main`.
3. Once the workflow succeeds, open the URL shown by the `github-pages`
   deployment. Check editing, package loading and PDF download.

The build job installs the Rust/Wasm toolchain and WASI SDK, builds a matching
initialized LaTeX format, checks the frontend, packages `dist/workspace/`, and
runs the browser smoke test under a project subpath. Rust builds, build tools
and checksum-verified TeX Live archives are cached between runs. Generated formats
are rebuilt with the current engine. The deployment job has the Pages and OIDC
permissions it needs; repository-wide workflow write permissions are unnecessary.

If a deployment ran before Pages was enabled, enable the setting and rerun the
failed deployment job. Forks must enable Pages in their own repository.

See GitHub's [custom workflow instructions](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)
for deployment details. The packaging script also writes `.nojekyll` for hosts
that support branch-based static publishing.

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
