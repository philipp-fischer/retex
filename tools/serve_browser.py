#!/usr/bin/env python3
"""Serve the demo and fetch missing standard files from pinned online TeX Live."""
import argparse
import functools
import http.server
import json
import re
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from texlive_resources import TexLiveStore
from workspace_browser import frontend_directory

PORT = Path(__file__).resolve().parents[1]

class ResourceHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, store, bundled=(), **kwargs):
        self.store, self.bundled = store, bundled
        super().__init__(*args, **kwargs)

    def do_GET(self):
        url = urlsplit(self.path)
        try:
            if url.path == '/texlive/resolve':
                parameters = parse_qs(url.query)
                if set(parameters) != {'kind', 'name'} or any(len(value) != 1 for value in parameters.values()):
                    self.send_error(400, 'Invalid resource request')
                    return
                result = self.store.resolve(parameters['kind'][0], parameters['name'][0], self.bundled)
                if result is None:
                    self.send_error(404, 'No unambiguous standard resource found')
                else:
                    self.send_bytes(json.dumps(result).encode(), 'application/json')
                return
            if url.path.startswith('/texlive/bundle/'):
                name = url.path.removeprefix('/texlive/bundle/')
                if not re.fullmatch(r'[a-f0-9]{64}\.bin', name):
                    self.send_error(400, 'Invalid bundle identity')
                    return
                path = self.store.cache/'bundles'/name
                if not path.is_file():
                    self.send_error(404, 'Bundle unavailable')
                else:
                    self.send_bytes(path.read_bytes(), 'application/octet-stream')
                return
            super().do_GET()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except (ValueError, OSError) as error:
            self.send_error(502, str(error))

    def send_bytes(self, data, content_type):
        self.send_response(200)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8000)
    parser.add_argument('--bind', default='127.0.0.1')
    parser.add_argument('--cache', type=Path, default=PORT/'.tools/texlive')
    parser.add_argument('--seed', type=Path, action='append', default=[])
    parser.add_argument('--offline', action='store_true', help='Use already downloaded archives only')
    args = parser.parse_args()
    build_path = PORT/'web/generated/build.json'
    build = json.loads(build_path.read_text()) if build_path.exists() else {}
    bundled = build.get('texlive', {}).get('packages', [])
    store = TexLiveStore(args.cache, args.seed, download=not args.offline)
    handler = functools.partial(ResourceHandler, directory=str(frontend_directory()), store=store, bundled=bundled)
    with http.server.ThreadingHTTPServer((args.bind, args.port), handler) as server:
        print(f'Interactive TeX: http://{args.bind}:{server.server_port}/index.html', flush=True)
        print(f'TeX Live 2024 resources: online archive -> {args.cache.resolve()}', flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass

if __name__ == '__main__':
    main()
