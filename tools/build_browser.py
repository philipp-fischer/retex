#!/usr/bin/env python3
"""Package the current shared engine and original font bytes for a local browser.

Requires a release Wasm build and matching wasm-bindgen CLI. Generated binary
assets live in web/generated; no earlier port engine or remote runtime is used.
"""
import argparse
import hashlib
import json
import subprocess
import shutil
from pathlib import Path
from texlive_resources import COMMON_PACKAGES, DATABASE_SHA256, RELEASE, TexLiveStore, merged_map, write_resource_packs

PORT = Path(__file__).resolve().parents[1]

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--wasm-only",action="store_true",help="Update only Wasm bindings and build hash, retaining the existing resource/format bundle")
    p.add_argument("--wasm-bindgen", type=Path, default=shutil.which("wasm-bindgen") or PORT / ".tools/wasm-bindgen/bin/wasm-bindgen")
    p.add_argument("--format",type=Path,help="Compatible initialized Rust format; created separately from original sources")
    p.add_argument("--latex",action="store_true",help="Package original LaTeX resources without rewriting their contents")
    p.add_argument("--common-papers", action="store_true", help="Add common paper packages and standard PDF fonts from pinned TeX Live")
    p.add_argument("--resource-cache", type=Path, default=PORT/".tools/texlive")
    p.add_argument("--resource-seed", type=Path, action="append", default=[], help="Reuse downloaded TeX Live archives after checksum verification")
    p.add_argument("--offline", action="store_true", help="Only use cached TeX Live archives")
    args = p.parse_args()
    if args.common_papers and not args.latex:
        p.error("--common-papers requires --latex")
    output = PORT / "web/generated"
    output.mkdir(parents=True, exist_ok=True)
    wasm = PORT / "target/wasm32-unknown-unknown/release/retex_rs2.wasm"
    subprocess.run([str(args.wasm_bindgen), "--target", "web", "--out-dir", str(output), str(wasm)], check=True)
    if args.wasm_only:
        identity = json.loads((output / "build.json").read_text())
        identity['sha256']['retex_rs2_bg.wasm'] = hashlib.sha256((output/'retex_rs2_bg.wasm').read_bytes()).hexdigest()
        identity['source_protocol_version'] = 2
        (output/'build.json').write_text(json.dumps(identity,indent=2)+'\n')
        print('Updated matching Wasm; resource and format bytes retained')
        return
    manifest, data, sources = [], bytearray(), []
    files = [(source, "/bundle/fonts/" + source.relative_to(PORT / "assets/fonts").as_posix())
             for source in sorted((PORT / "assets/fonts").rglob("*")) if source.is_file()]
    files.extend((PORT / "assets/tex" / name, "/bundle/tex/" + name) for name in ("plain.tex","hyphen.tex"))
    if args.latex:
        root=PORT/'assets/latex-texmf'
        files.extend((source,'/bundle/'+source.relative_to(root).as_posix()) for source in sorted(root.rglob('*')) if source.is_file())
    texlive = None
    additional, maps = {}, []
    if args.common_papers:
        fixture = json.loads((PORT/'assets/latex-texmf/manifest.json').read_text())
        bundled = fixture.get('package_database', {}).get('dependency_closure', [])
        store = TexLiveStore(args.resource_cache, args.resource_seed, download=not args.offline)
        additional, maps, packages = store.collect(COMMON_PACKAGES, exclude=bundled)
        texlive = {'release': RELEASE, 'database_sha256': DATABASE_SHA256,
                   'roots': COMMON_PACKAGES, 'packages': sorted(set(bundled)|set(packages)),
                   'archives': packages}
        texlive['index_sha256'] = write_resource_packs(store, output/'texlive', texlive['packages'])
        print('Adding', len(additional), 'runtime files from', len(packages), 'common-paper packages', flush=True)
    initialized=None
    if args.format:
        path='/bundle/formats/initialized.rsfmt'
        files.append((args.format.resolve(),path))
        initialized={'path':path,'sha256':hashlib.sha256(args.format.read_bytes()).hexdigest()}
    paths=[path for _,path in files]
    if len(paths)!=len(set(paths)):raise ValueError('Duplicate package resource paths')
    combined_map = None
    if args.common_papers:
        combined_map = merged_map((PORT/'assets/fonts/assets/pdftex.map').read_bytes(), additional, maps)
        (output/'common-fonts.map').write_bytes(combined_map)
    resources = [(source.read_bytes(), path) for source, path in files if source.is_file()]
    resources.extend((raw, '/bundle/texlive/'+path) for path, raw in sorted(additional.items()))
    for raw, path in resources:
        if combined_map is not None and path == '/bundle/fonts/assets/pdftex.map':
            raw = combined_map
        manifest.append({"path": list(path.encode()), "id": path, "version": 1, "offset": len(data), "length": len(raw)})
        sources.append({"path": path, "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)})
        data.extend(raw)
    (output / "bundle.json").write_text(json.dumps(manifest,separators=(",", ":")) + "\n")
    (output / "bundle.bin").write_bytes(data)
    hashes = {name:hashlib.sha256((output / name).read_bytes()).hexdigest() for name in ("retex_rs2_bg.wasm","bundle.json","bundle.bin")}
    (output / "build.json").write_text(json.dumps({"schema_version":1,"source_protocol_version":2,"sha256":hashes,"resources":sources,'initialized_format':initialized,
                                                 **({'texlive':texlive} if texlive else {})},indent=2)+"\n")
    print("Packaged",len(manifest),"resources,",len(data),"bytes; Wasm",(output / "retex_rs2_bg.wasm").stat().st_size,"bytes")

if __name__ == "__main__":
    main()
