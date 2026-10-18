"""Focused integrity and dependency tests for standard-resource provisioning."""
import hashlib
import io
import lzma
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import texlive_resources as resources

class TexLiveResourcesTests(unittest.TestCase):
    def test_database_identity_and_runtime_paths(self):
        raw = lzma.compress(b'name example\ndepend helper\nrunfiles size=1\n RELOC/tex/latex/example.sty\n RELOC/fonts/tfm/example.tfm\ndocfiles size=1\n RELOC/doc/example.pdf\n\n')
        with patch.object(resources, 'DATABASE_SHA256', hashlib.sha256(raw).hexdigest()):
            record = resources.parse_database(raw)['example']
            self.assertEqual(record['runtime_files'], ['tex/latex/example.sty', 'fonts/tfm/example.tfm'])
            with self.assertRaises(ValueError):
                resources.parse_database(raw+b'corrupt')
        for path in ['/absolute.sty', 'RELOC/../escape.sty']:
            with self.assertRaises(ValueError):
                resources.runtime_path(path)

    def test_dependency_cycles_and_platform_exclusion(self):
        with tempfile.TemporaryDirectory() as directory:
            store = resources.TexLiveStore(directory, download=False)
            store._records = {'root': {'depend': ['helper', 'tool.ARCH']}, 'helper': {'depend': ['root']}}
            self.assertEqual(store.closure(['root']), ['helper', 'root'])
            with self.assertRaises(ValueError):
                store.closure(['unknown'])

    def test_seed_archive_is_verified_before_caching(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'example.tar.xz').write_bytes(b'original')
            store = resources.TexLiveStore(root/'cache', seeds=[root], download=False)
            digest = hashlib.sha512(b'original').hexdigest()
            self.assertEqual(store._archive('example.tar.xz', digest, 'sha512'), b'original')
            (root/'cache/archives/example.tar.xz').write_bytes(b'corrupt')
            with self.assertRaises(ValueError):
                store._archive('example.tar.xz', digest, 'sha512')

    def test_archives_cannot_replace_expected_files_with_links(self):
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode='w:xz') as archive:
            entry = tarfile.TarInfo('RELOC/tex/latex/example.sty')
            entry.type = tarfile.SYMTYPE
            entry.linkname = '/etc/passwd'
            archive.addfile(entry)
        with tempfile.TemporaryDirectory() as directory:
            store = resources.TexLiveStore(directory, download=False)
            store._records = {'example': {'containerchecksum': ['unused'], 'runtime_files': ['tex/latex/example.sty']}}
            with patch.object(store, '_archive', return_value=raw.getvalue()):
                with self.assertRaises(ValueError):
                    store.package('example')

    def test_resource_lookup_does_not_guess_or_fetch_project_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            store = resources.TexLiveStore(directory, download=False)
            store._records = {
                'one': {'runtime_files': ['tex/latex/shared.sty', 'fonts/tfm/face.tfm']},
                'two': {'runtime_files': ['tex/latex/other/shared.sty']},
            }
            self.assertEqual(store.locate('tfm', 'face'), ('one', 'fonts/tfm/face.tfm'))
            self.assertIsNone(store.locate('tex', 'shared.sty'))
            for kind, name in [('tex', '../private.tex'), ('image', 'figure.png'), ('tex', 'something.py'), ('tfm', 'face.sty')]:
                self.assertIsNone(store.locate(kind, name))

    def test_font_map_precedence_and_bundle_hashes(self):
        files = {'fonts/map/add.map': b'first Other <other.pfb\nsecond Original <second.pfb\n'}
        merged = resources.merged_map(b'% comment\nfirst Original <first.pfb\n', files, files)
        self.assertEqual(merged, b'first Original <first.pfb\nsecond Original <second.pfb\n')
        descriptor, data = resources.resource_bundle(files, list(files), {'example': {}})
        self.assertEqual(descriptor['sha256'], hashlib.sha256(data).hexdigest())
        self.assertEqual(descriptor['manifest'][0]['sha256'], hashlib.sha256(data).hexdigest())

if __name__ == '__main__':
    unittest.main()
