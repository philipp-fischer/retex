"""Explicit CodeMirror fixture adapter; measured input still uses real keyboard events."""
from pathlib import Path
PORT=Path(__file__).resolve().parents[1]
def frontend_directory():
    output=PORT/'dist/workspace'
    if not (output/'index.html').is_file():
        raise RuntimeError('Package the workspace first: cd web && npm ci && npm run package')
    return output

def set_source(page,text):
    page.evaluate('text=>editorProbe.editor.replace(text)',text)
