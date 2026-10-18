#!/usr/bin/env python3
"""Exercise a static ReTeX deployment under a project URL, without a backend."""
import argparse
import functools
import http.server
import json
import tempfile
import threading
from pathlib import Path
from urllib.parse import urlsplit

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
PREFIX = "/retex-demo/"
SETTLED = "window.editorProbe && editorProbe.complete === editorProbe.revision && !document.querySelector('#export').disabled && document.querySelector('#viewport').dataset.stale === 'false'"
COMPILED = "window.editorProbe && !document.querySelector('#export').disabled && document.querySelector('#status').dataset.busy === 'false'"


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if not urlsplit(self.path).path.startswith(PREFIX):
            self.send_error(404)
            return
        self.path = self.path[len(PREFIX) - 1:]
        super().do_GET()

    def log_message(self, *_):
        pass


def transparency_pdf():
    """Small original fixture; no external PDF tools or third-party artwork."""
    paint = b"0 .4 .8 rg 10 10 80 40 re f\n"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 60] /Resources <<>> /Group << /S /Transparency /CS /DeviceRGB /I true >> /Contents 4 0 R >>",
        b"<< /Length " + str(len(paint)).encode() + b" >>\nstream\n" + paint + b"endstream",
    ]
    result = bytearray(b"%PDF-1.4\n")
    offsets = [0]
    for number, value in enumerate(objects, 1):
        offsets.append(len(result))
        result.extend(f"{number} 0 obj\n".encode() + value + b"\nendobj\n")
    start = len(result)
    result.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
    for offset in offsets[1:]:
        result.extend(f"{offset:010d} 00000 n \n".encode())
    result.extend(f"trailer\n<< /Root 1 0 R /Size {len(offsets)} >>\nstartxref\n{start}\n%%EOF\n".encode())
    return bytes(result)


def scroll_preview_page(page, number):
    # A page can be replaced between layout passes. Resolve and scroll in the
    # same browser task, without retaining Playwright's handle to an old node.
    page.wait_for_function("""number => {
        const slot = document.querySelector(`#preview > [data-page="${number}"]`);
        if (!slot) return false;
        slot.scrollIntoView({block: 'center', inline: 'nearest', behavior: 'instant'});
        return true;
    }""", arg=number)


def check_page_error(page, work, report):
    """Unsupported PDF graphics keep their bounds and the surrounding page."""
    previous = page.evaluate("editorProbe.revision")
    page.locator("#project-file").set_input_files({
        "name": "transparency.pdf", "mimeType": "application/pdf", "buffer": transparency_pdf(),
    })
    page.wait_for_function("r => editorProbe.revision > r", arg=previous)
    page.wait_for_function(SETTLED, timeout=180000)
    source = "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\nFirst page.\\newpage\nSecond page. FIGURE\\newpage\nThird page.\\end{document}\n"
    # Cover adding, removing and restoring a graphic with unsupported PDF paint.
    for include_figure in [True, False, True]:
        previous = page.evaluate("editorProbe.revision")
        figure = "\\includegraphics{transparency.pdf}" if include_figure else ""
        page.evaluate("s => editorProbe.editor.replace(s)", source.replace("FIGURE", figure))
        page.wait_for_function("r => editorProbe.revision > r", arg=previous)
        # Toolbar presentation deliberately retains the previous state briefly
        # during typing. Wait for this revision's actual engine completion too.
        page.wait_for_function("editorProbe.settlementMeasurements.some(m => m.revision === editorProbe.revision)", timeout=180000)
        # Graphic placeholders still allow the surrounding page to settle.
        page.wait_for_function(COMPILED, timeout=180000)
        scroll_preview_page(page, 2)
        page.wait_for_function("editorProbe.renderer.pages.get(2)?.revision === editorProbe.revision")
        assert page.locator(".preview-page-error").count() == 0
        if not include_figure:
            assert page.evaluate("editorProbe.renderer.pages.get(2).pdfPlaceholders.length") == 0
            continue
        page.wait_for_function("editorProbe.renderer.pages.get(2)?.pdfPlaceholders.length === 1")
        warning = page.evaluate("editorProbe.renderer.pages.get(2).pdfPlaceholders[0]")
        assert "Group" in warning["message"]
        assert warning["label"] == "transparency.pdf"
        assert page.evaluate("editorProbe.renderer.pages.get(2).glyphs.length") > 0
        for zoom in [False, True]:
            if zoom:
                width = page.evaluate("editorProbe.renderer.pages.get(2).size.width")
                page.locator("#zoom-out").click()
                page.wait_for_function("width => editorProbe.renderer.pages.get(2).size.width !== width", arg=width)
            quad = page.evaluate("editorProbe.renderer.pages.get(2).pdfPlaceholders[0].quad")
            assert abs(max(p[0] for p in quad) - min(p[0] for p in quad) - 100) < .1
            assert abs(max(p[1] for p in quad) - min(p[1] for p in quad) - 60) < .1
        assert page.locator("#export").is_visible()
        assert page.locator(".cm-content").is_visible()
    # The original PDF graphic remains in exported output.
    page.wait_for_function(COMPILED, timeout=10000)
    with page.expect_download(timeout=30000) as event:
        page.locator("#export").click()
    output = Path(work) / "preview-limitation.pdf"
    event.value.save_as(output)
    assert output.read_bytes().startswith(b"%PDF-")
    assert b"/Group" in output.read_bytes(), "Export lost the PDF transparency group"
    scroll_preview_page(page, 3)
    page.wait_for_function("document.querySelector('#preview > canvas[data-page=\"3\"]')?.dataset.revision === String(editorProbe.revision)")
    assert page.locator('#preview > canvas[data-page="1"]').count() == 1
    report["pdf_placeholder"] = True
    # Keep coverage for whole-page failures unrelated to imported PDF graphics.
    page.evaluate("editorProbe.renderer.pageError(3, 'Preview unavailable: test renderer limitation')")
    bounds = page.evaluate("() => { const el = document.querySelector('.preview-page-error'), n = el.getBoundingClientRect(), p = el.parentElement.getBoundingClientRect(); return {inside: n.left >= p.left && n.top >= p.top && n.right <= p.right && n.bottom <= p.bottom, parent: el.offsetParent === el.parentElement}; }")
    assert bounds == {"inside": True, "parent": True}, bounds
    report["page_error_scoped"] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--site", type=Path, default=ROOT / "dist/workspace")
    parser.add_argument("--chromium", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--live-packages", action="store_true", help="Also test arbitrary package downloads from the current public TeX Live repository")
    args = parser.parse_args()
    if not (args.site / "generated/build.json").is_file():
        parser.error("Build and package the browser app first (see docs/BUILDING.md)")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(args.site.resolve())))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f"http://127.0.0.1:{server.server_port}"
    report = {"url": origin + PREFIX, "errors": [], "http_failures": [], "external_requests": [], "package_requests": []}
    try:
        with sync_playwright() as pw, tempfile.TemporaryDirectory(prefix="retex-browser-smoke-") as work:
            options = {"headless": True}
            if args.chromium:
                options["executable_path"] = str(args.chromium.resolve())
            browser = pw.chromium.launch(**options)
            page = browser.new_page(accept_downloads=True)
            page.on("pageerror", lambda error: report["errors"].append(str(error)))
            page.on("response", lambda response: report["http_failures"].append({"url": response.url, "status": response.status}) if response.status >= 400 else None)
            downloads = []

            def route_request(route):
                if not route.request.url.startswith(origin + "/"):
                    if args.live_packages and urlsplit(route.request.url).netloc in {"mirrors.huaweicloud.com", "repo.huaweicloud.com"}:
                        report["package_requests"].append(route.request.url)
                        route.continue_()
                        return
                    report["external_requests"].append(route.request.url)
                    route.abort()
                elif "/generated/texlive/" in route.request.url and urlsplit(route.request.url).path.endswith(".bin"):
                    downloads.append(route)
                else:
                    route.continue_()

            page.context.route("**/*", route_request)
            page.goto(report["url"])
            page.wait_for_function(SETTLED, timeout=180000)
            assert page.locator("#source-wrap").get_attribute("aria-pressed") == "true"
            assert page.evaluate("editorProbe.editor.view.lineWrapping") is True
            revision = page.evaluate("editorProbe.revision")
            page.locator("#source-wrap").click()
            page.wait_for_function("!editorProbe.editor.view.lineWrapping")
            page.locator("#source-wrap").click()
            page.wait_for_function("editorProbe.editor.view.lineWrapping")
            assert page.evaluate("editorProbe.revision") == revision
            # cancel is intentionally outside the base bundle. Pasting a new
            # declaration must discover it on the resident source-edit path.
            source = "\\documentclass{article}\n\\usepackage{amsmath,xcolor,cancel}\n\\begin{document}\n\\section{Browser test}\nLive typesetting: $e^{i\\pi}+1=0$.\n\\textcolor{blue}{Local compilation.} $\\cancel{x}+y$\n\\end{document}\n"
            previous = page.evaluate("editorProbe.revision")
            with page.expect_request("**/generated/texlive/*.bin", timeout=30000):
                page.evaluate("text => editorProbe.editor.replace(text)", source)
            page.wait_for_function("revision => editorProbe.revision > revision", arg=previous)
            page.wait_for_function("document.querySelector('#status').dataset.busy === 'true'")
            assert "Downloading cancel.sty" in page.locator("#status").inner_text()
            assert not page.locator("#cancel").is_disabled(), "Download cannot be cancelled"
            assert page.locator("#export").is_disabled(), "Stale PDF export enabled during download"
            report["download_status"] = page.locator("#status").inner_text()
            # Working edits during the held download must survive acceptance.
            source = source.replace("Local compilation.", "Edited during download.")
            page.evaluate("text => editorProbe.editor.replace(text)", source)
            assert downloads, "Package binary request was not held"
            downloads.pop().continue_()
            page.wait_for_function(SETTLED, timeout=180000)
            assert page.evaluate("editorProbe.editor.value") == source
            assert page.locator("#status").get_attribute("data-busy") == "false"
            assert page.locator("#preview canvas").count() > 0, "No preview canvas"
            # A real keystroke inside a visible paragraph must produce a new completed revision.
            page.locator(".cm-content").click()
            page.keyboard.press("Control+Home")
            page.keyboard.press("ArrowDown")
            page.keyboard.press("ArrowDown")
            page.keyboard.press("ArrowDown")
            page.keyboard.press("ArrowDown")
            previous = page.evaluate("editorProbe.revision")
            page.keyboard.type("A")
            page.wait_for_function("revision => editorProbe.revision > revision", arg=previous)
            page.wait_for_function(SETTLED, timeout=180000)
            if args.live_packages:
                # Computed names intentionally bypass literal preload hints.
                # LaTeX must request required missing inputs during execution.
                source = "\\def\\paperclass{llncs}\n\\documentclass{\\paperclass}\n\\def\\counterpackage{aliascnt}\n\\usepackage{\\counterpackage}\n\\begin{document}\n\\title{Direct repository test}\n\\author{Ada Lovelace}\n\\institute{Example Institute}\n\\maketitle\n\\begin{abstract}Classes and dependencies load from TeX Live.\\end{abstract}\n\\section{Example}\nThis document needs no compilation server.\n\\end{document}\n"
                previous = page.evaluate("editorProbe.revision")
                page.evaluate("text => editorProbe.editor.replace(text)", source)
                page.wait_for_function("revision => editorProbe.revision > revision", arg=previous)
                page.wait_for_function(SETTLED, timeout=180000)
                for package in ["llncs", "aliascnt"]:
                    assert any(urlsplit(url).path.endswith(f"/{package}.tar.xz") for url in report["package_requests"]), f"{package} was not downloaded directly"
                page.locator("#storage-settings").click()
                page.get_by_role("button", name="Remove llncs from cache", exact=True).wait_for()
                page.get_by_role("button", name="Remove aliascnt from cache", exact=True).wait_for()
                page.get_by_role("button", name="Remove llncs from cache", exact=True).click()
                page.wait_for_function("!document.querySelector('button[aria-label=\"Remove llncs from cache\"]')")
                assert page.get_by_role("button", name="Remove aliascnt from cache", exact=True).count() == 1
                page.get_by_role("button", name="Close storage settings", exact=True).click()
                report["live_packages"] = True
            assert page.locator("footer").count() == 0, "Deprecated mode footer remains"
            with page.expect_download(timeout=30000) as event:
                page.locator("#export").click()
            pdf = Path(work) / "result.pdf"
            event.value.save_as(pdf)
            assert pdf.read_bytes().startswith(b"%PDF-"), "Export is not a PDF"
            report.update(browser=browser.version, revision=page.evaluate("editorProbe.revision"), pdf_bytes=pdf.stat().st_size)
            check_page_error(page, work, report)
            browser.close()
        assert not report["errors"], report["errors"]
        assert not report["http_failures"], report["http_failures"]
        assert not report["external_requests"], report["external_requests"]
        report["passed"] = True
    finally:
        server.shutdown()
        server.server_close()
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
