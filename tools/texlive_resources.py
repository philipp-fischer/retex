"""Verified runtime resources from the same frozen TeX Live as our LaTeX format."""
import hashlib
import io
import json
import lzma
import re
import tarfile
import threading
import urllib.request
from pathlib import Path, PurePosixPath

BASE = 'https://ftp.math.utah.edu/pub/tex/historic/systems/texlive/2024/tlnet-final/'
DATABASE_SHA256 = '60b9f8f8893d591d220d2f4f8dda3f686caa165efe78aa0ccc8dec80d48799aa'
RELEASE = 'texlive-2024-final'
COMMON_PACKAGES = (
    'psnfss', 'dvips', 'texlive-scripts', 'helvetic', 'times', 'courier', 'avantgar', 'bookman', 'ncntrsbk',
    'palatino', 'symbol', 'zapfchan', 'zapfding', 'lm', 'rsfs', 'wasy',
    'wasy-type1', 'txfonts', 'pxfonts', 'newtx', 'tex-gyre',
    'caption', 'float', 'fancyhdr', 'listings', 'mathtools', 'algorithms',
    'algorithmicx', 'wrapfig', 'placeins', 'subfig', 'subfigure', 'titlesec',
    'tocloft', 'setspace', 'ulem', 'url', 'cite', 'csquotes', 'xkeyval',
    'adjustbox', 'collectbox', 'threeparttable', 'comment', 'footmisc',
    'appendix', 'preprint', 'revtex', 'ieeetran', 'elsarticle',
)
RESOURCE_PACKAGES = (
    'siunitx', 'cleveref', 'standalone', 'pgfplots', 'beamer', 'colortbl',
    'tabulary', 'tabularray', 'tcolorbox', 'mdframed', 'needspace', 'nicematrix',
    'physics', 'ntheorem', 'cancel', 'xurl', 'xstring', 'fancybox', 'fancyvrb',
    'framed', 'soul', 'blindtext', 'lipsum', 'gensymb',
)
FONT_SUFFIXES = {'.tfm', '.vf', '.pfb', '.enc', '.map', '.sfd'}
KINDS = {'tex', 'tfm', 'vf', 'map', 'enc files', 'type1 fonts'}
MAX_ARCHIVE = 64 * 1024 * 1024
MAX_RUNTIME = 64 * 1024 * 1024
# resolve_resource's JSON transport expands byte buffers to integer arrays.
MAX_AUTOMATIC = 12 * 1024 * 1024


def runtime_path(name):
    path = PurePosixPath(name)
    if path.is_absolute() or '..' in path.parts or not path.parts:
        raise ValueError('Unsafe TeX Live archive path')
    if path.parts[0] in ('RELOC', 'texmf-dist'):
        path = PurePosixPath(*path.parts[1:])
    if not path.parts:
        return None
    if path.parts[0] == 'tex' or (path.parts[0] == 'fonts' and path.suffix.lower() in FONT_SUFFIXES):
        return str(path)
    return None


def parse_database(raw):
    if hashlib.sha256(raw).hexdigest() != DATABASE_SHA256:
        raise ValueError('Pinned TeX Live database checksum differs')
    records = {}
    for record in lzma.decompress(raw).decode().split('\n\n'):
        if not record.startswith('name '):
            continue
        fields, runfiles, section = {}, [], None
        for line in record.splitlines():
            if line.startswith(' '):
                if section == 'runfiles':
                    path = runtime_path(line.strip())
                    if path:
                        runfiles.append(path)
            elif line:
                key, _, value = line.partition(' ')
                section = key
                fields.setdefault(key, []).append(value)
        fields['runtime_files'] = runfiles
        records[fields['name'][0]] = fields
    return records


class TexLiveStore:
    def __init__(self, cache, seeds=(), download=True):
        self.cache = Path(cache)
        self.seeds = tuple(Path(seed) for seed in seeds)
        self.download = download
        self._records = None
        self._index = None
        self.lock = threading.RLock()

    def _archive(self, filename, checksum, algorithm):
        path = self.cache/'archives'/filename
        with self.lock:
            raw = path.read_bytes() if path.exists() else None
            if raw is None:
                for seed in self.seeds:
                    source = seed/filename
                    if source.is_file():
                        raw = source.read_bytes()
                        break
            if raw is None:
                if not self.download:
                    raise FileNotFoundError('Resource is not cached: '+filename)
                url = BASE + ('tlpkg/' if filename == 'texlive.tlpdb.xz' else 'archive/') + filename
                request = urllib.request.Request(url, headers={'User-Agent': 'retex-rs2/TeXLive-resources'})
                with urllib.request.urlopen(request, timeout=30) as response:
                    raw = response.read(MAX_ARCHIVE+1)
            if len(raw) > MAX_ARCHIVE or hashlib.new(algorithm, raw).hexdigest() != checksum:
                raise ValueError('TeX Live archive checksum or size differs: '+filename)
            if not path.exists():
                path.parent.mkdir(parents=True, exist_ok=True)
                temporary = path.with_suffix('.tmp')
                temporary.write_bytes(raw)
                temporary.replace(path)
            return raw

    @property
    def records(self):
        with self.lock:
            if self._records is None:
                self._records = parse_database(self._archive('texlive.tlpdb.xz', DATABASE_SHA256, 'sha256'))
            return self._records

    def closure(self, roots):
        pending, names = list(roots), set()
        while pending:
            name = pending.pop()
            if name.endswith('.ARCH') or name in names:
                continue
            if name not in self.records:
                raise ValueError('Unknown TeX Live package: '+name)
            names.add(name)
            pending.extend(self.records[name].get('depend', []))
        return sorted(names)

    def package(self, name):
        fields = self.records[name]
        if 'containerchecksum' not in fields:
            return {}
        raw = self._archive(name+'.tar.xz', fields['containerchecksum'][0], 'sha512')
        files, total = {}, 0
        allowed = set(fields['runtime_files'])
        with tarfile.open(fileobj=io.BytesIO(raw), mode='r:xz') as archive:
            for member in archive:
                if member.isdir():
                    continue
                path = runtime_path(member.name)
                if path is None:
                    continue
                if path not in allowed or not member.isfile() or member.size < 0:
                    raise ValueError('Unexpected runtime archive member: '+member.name)
                total += member.size
                if total > MAX_RUNTIME or path in files:
                    raise ValueError('TeX Live runtime package budget exceeded')
                files[path] = archive.extractfile(member).read()
        if set(files) != allowed:
            raise ValueError('TeX Live runtime manifest differs: '+name)
        return files

    def collect(self, roots, exclude=()):
        names = [name for name in self.closure(roots) if name not in set(exclude)]
        files, maps, packages = {}, [], {}
        for name in names:
            fields = self.records[name]
            for path, raw in self.package(name).items():
                if path in files and files[path] != raw:
                    raise ValueError('Conflicting TeX Live runtime file: '+path)
                files[path] = raw
            for execute in fields.get('execute', []):
                match = re.fullmatch(r'add(?:Mixed)?Map (\S+)', execute)
                if match:
                    candidates = [path for path in fields['runtime_files'] if PurePosixPath(path).name == match[1]]
                    if len(candidates) != 1:
                        raise ValueError('Ambiguous font map: '+execute)
                    maps.append(candidates[0])
            # updmap's default pdftexDownloadBase14=true uses these original
            # entries to bind Adobe metric names to the distributed URW fonts.
            base35 = 'fonts/map/dvips/tetex/ps2pk35.map'
            if base35 in fields['runtime_files']:
                maps.append(base35)
            packages[name] = {'revision': fields.get('revision', [''])[0],
                              'sha512': fields.get('containerchecksum', [''])[0],
                              'url': BASE+'archive/'+name+'.tar.xz',
                              'license': fields.get('catalogue-license', [''])[0]}
        return files, sorted(set(maps)), packages

    def locate(self, kind, name):
        if kind not in KINDS or not re.fullmatch(r'[A-Za-z0-9_+.-]{1,200}', name) or name in ('.', '..'):
            return None
        suffix = {'tfm': '.tfm', 'vf': '.vf', 'map': '.map', 'enc files': '.enc', 'type1 fonts': '.pfb', 'tex': '.tex'}[kind]
        if '.' not in name:
            name += suffix
        if kind != 'tex' and not name.lower().endswith(suffix):
            return None
        if kind == 'tex' and PurePosixPath(name).suffix.lower() not in {'.tex', '.sty', '.cls', '.def', '.cfg', '.fd', '.clo', '.ltx'}:
            return None
        if self._index is None:
            self._index = {}
            for package, fields in self.records.items():
                for path in fields['runtime_files']:
                    self._index.setdefault(PurePosixPath(path).name, []).append((package, path))
        candidates = [item for item in self._index.get(name, []) if item[1].startswith('tex/' if kind == 'tex' else 'fonts/')]
        # Do not guess when two packages provide different files with one name.
        return candidates[0] if len(candidates) == 1 else None

    def resolve(self, kind, name, bundled=()):
        with self.lock:
            found = self.locate(kind, name)
            if found is None:
                return None
            package, requested = found
            # Include its whole dependency closure: optional VF probes and font
            # map selection must see companions before execution restarts.
            files, maps, packages = self.collect([package], exclude=set(bundled)-{package})
            if sum(map(len, files.values())) > MAX_AUTOMATIC:
                raise ValueError('Package is too large for automatic loading; supply the resource manually')
            descriptor, data = resource_bundle(files, maps, packages)
            descriptor['requested'] = '/bundle/texlive/'+requested
            digest = descriptor['sha256']
            descriptor['url'] = './texlive/bundle/'+digest+'.bin'
            target = self.cache/'bundles'/str(digest+'.bin')
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            return descriptor


def resource_bundle(files, maps, packages):
    data, manifest = bytearray(), []
    for path, raw in sorted(files.items()):
        manifest.append({'path': '/bundle/texlive/'+path, 'offset': len(data), 'length': len(raw),
                         'sha256': hashlib.sha256(raw).hexdigest()})
        data.extend(raw)
    if len(data) > MAX_AUTOMATIC:
        raise ValueError('Resource pack exceeds the automatic-loading budget')
    digest = hashlib.sha256(data).hexdigest()
    return {'release': RELEASE, 'database_sha256': DATABASE_SHA256, 'packages': packages,
            'maps': ['/bundle/texlive/'+path for path in maps], 'manifest': manifest,
            'sha256': digest}, data


def write_resource_packs(store, output, bundled, roots=RESOURCE_PACKAGES):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    index = {}
    for root in roots:
        files, maps, packages = store.collect([root], exclude=bundled)
        if not files:
            continue
        descriptor, data = resource_bundle(files, maps, packages)
        digest = descriptor['sha256']
        descriptor['url'] = './generated/texlive/'+digest+'.bin'
        (output/(digest+'.bin')).write_bytes(data)
        descriptor_name = root+'-'+digest+'.json'
        (output/descriptor_name).write_text(json.dumps(descriptor, separators=(',', ':'))+'\n')
        for path in store.records[root]['runtime_files']:
            name = PurePosixPath(path).name
            previous = index.get(name)
            target = './generated/texlive/'+descriptor_name
            index[name] = target if previous is None or previous == target else False
        print('Prepared static resource pack', root, len(data), 'bytes', flush=True)
    (output/'index.json').write_text(json.dumps({'release': RELEASE, 'database_sha256': DATABASE_SHA256,
                                               'files': index}, separators=(',', ':'))+'\n')
    return hashlib.sha256((output/'index.json').read_bytes()).hexdigest()


def merged_map(base, files, maps):
    # Preserve existing exact map entries; add each new TeX font once.
    lines, seen = [], set()
    for raw in [base, *(files[path] for path in sorted(maps))]:
        for line in raw.decode('latin1').splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith(('%', '#', ';')):
                continue
            name = stripped.split()[0]
            if name not in seen:
                lines.append(line)
                seen.add(name)
    return ('\n'.join(lines)+'\n').encode('latin1')
