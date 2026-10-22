#!/usr/bin/env python3
"""Capture real Kestrel windows with generated demo files and the active theme.

Requires cargo, Xvfb, ImageMagick, librsvg and libXtst. Uses an isolated X display
and XDG state directory; never changes the desktop theme or Kestrel session.
Run from any directory: python3 scripts/screenshots.py
"""
from __future__ import annotations

import argparse
import ctypes as C
import os
from pathlib import Path
import shutil
import subprocess as sp
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
OUTPUT = REPO / "assets" / "screenshots"
SIZE = (1600, 900)


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def fixtures(studio: Path) -> None:
    project = studio / "Projects" / "Aurora"
    code = '''import { useState } from "react";
import { Gallery } from "./components/Gallery";
import { Header } from "./components/Header";
import { collections } from "./data/collections";

// Aurora — a small home for big ideas.
export function App() {
  const [active, setActive] = useState("all");
  const [query, setQuery] = useState("");

  const visible = collections.filter((item) => {
    const inCollection = active === "all"
      || item.collection === active;
    const matchesQuery = item.title
      .toLowerCase()
      .includes(query.toLowerCase());

    return inCollection && matchesQuery;
  });

  return (
    <main className="studio">
      <Header
        title="Make room for discovery."
        subtitle="A collection of places worth a second look."
        onSearch={setQuery}
      />
      <nav aria-label="Collections">
        {["all", "alpine", "coastal"].map((name) => (
          <button
            key={name}
            aria-pressed={active === name}
            onClick={() => setActive(name)}
          >
            {name}
          </button>
        ))}
      </nav>
      <Gallery items={visible} />
      <footer>Designed slowly. Built with care.</footer>
    </main>
  );
}
'''
    write(project / "Website" / "src" / "App.tsx", code)
    write(project / "Website" / "src" / "Gallery.tsx", 'export function Gallery({ items }) {\n  return items.map((item) => <img key={item.id} src={item.image} alt={item.title} />);\n}\n')
    for name in ['Card', 'Header', 'Layout', 'Search', 'Collection']:
        write(project / 'Website' / 'src' / f'{name}.tsx', f'// Aurora / {name} component\nexport function {name}({{ children }}) {{\n  return <section>{{children}}</section>;\n}}\n')
    write(project / "Website" / "src" / "theme.css", ':root {\n  --ink: #060b1e;\n  --paper: #ffcead;\n  --accent: #7d82d9;\n}\n')
    write(project / "Website" / "package.json", '{"name":"aurora-studio","version":"1.0.0","private":true,"scripts":{"dev":"vite","build":"vite build"}}\n')
    write(project / "app.ts", code)
    write(project / "launch-plan.md", '# Aurora / launch plan\n\nA new collection of places worth a second look.\n\n## This week\n\n- Finish the alpine collection\n- Review the coastal colour studies\n- Ship the accessible gallery\n- Prepare the autumn launch\n\n## Team\n\nNaDya — art direction\nSam — engineering\nAlex — photography\n\n## Milestones\n\nDesign review: October 22\nPreview release: October 28\nPublic launch: November 4\n')
    write(project / "project.json", '{"name":"Aurora","status":"in progress","owner":"NaDya","collections":["Alpine","Coastal","Desert"],"deliverables":{"images":9,"website":1,"brandGuide":1},"palette":{"ink":"#060b1e","accent":"#7d82d9","paper":"#ffcead"},"launch":"2026-11-04"}\n')
    write(project / "Research" / "audience.csv", 'segment,participants,preference,score\nExplorers,48,Alpine,9.2\nCollectors,32,Coastal,8.9\nDesigners,26,Desert,9.4\nTravellers,54,Coastal,9.1\n')
    write(project / "Research" / "interview-notes.md", '# Field notes\n\nPeople want less noise and more room to explore.\n\n- Clear names beat clever labels.\n- A good preview saves a round trip.\n- Keep the path back visible.\n')
    for name, text in {
        'brand-notes.md': '# Aurora\n\nQuiet, curious, considered.\n',
        'checklist.md': '# Delivery checklist\n\n- [x] Design system\n- [x] Image exports\n- [ ] Accessibility review\n',
        'colours.css': ':root { --ink: #060b1e; --accent: #7d82d9; --paper: #ffcead; }\n',
        'credits.txt': 'All names, artwork and project details in this workspace are fictional.\n',
        'feedback.md': '# Review\n\nThe alpine studies are ready for the next round.\n',
        'release-notes.md': '# Preview 01\n\nA first look at the Aurora collection.\n',
        'schedule.csv': 'phase,owner,status\nResearch,Alex,Complete\nDesign,NaDya,Complete\nBuild,Sam,In progress\n',
    }.items():
        write(project / name, text)
    for name in ['Meridian', 'Northstar']:
        write(studio / 'Projects' / name / 'brief.md', f'# {name}\n\nA fictional studio project.\n')
    write(studio / 'Deliverables' / 'handoff.md', '# Ready for handoff\n\nFinal artwork and documentation for the autumn collection.\n')
    for folder in ['Assets', 'Notes']:
        (studio / folder).mkdir(parents=True, exist_ok=True)

    # Original SVG studies rendered to PNG so Kestrel shows actual image thumbnails.
    scenes = [
        ('01-alpine-dawn', 'ALPINE DAWN', '#b2a4dd', '#f0bc94', '#263256', '#111d38'),
        ('02-coastal-haze', 'COASTAL HAZE', '#96b4c2', '#f4d0a3', '#456f82', '#203d55'),
        ('03-desert-light', 'DESERT LIGHT', '#dba681', '#f7d9a1', '#aa6e67', '#643d56'),
        ('04-blue-hour', 'BLUE HOUR', '#6979bb', '#b4a4d2', '#303d78', '#111f4a'),
        ('05-salt-and-stone', 'SALT & STONE', '#b3c6c0', '#ecd7b4', '#638e8b', '#254e60'),
        ('06-soft-horizon', 'SOFT HORIZON', '#cbb6cf', '#edc7b0', '#8a779e', '#444e7d'),
        ('07-ember-valley', 'EMBER VALLEY', '#c08c91', '#ecc49b', '#906273', '#4c3659'),
        ('08-northern-sky', 'NORTHERN SKY', '#94a6c9', '#d0c3d8', '#647c9c', '#283f65'),
        ('09-golden-tide', 'GOLDEN TIDE', '#c4b594', '#f4d5a4', '#939f91', '#475e71'),
    ]
    for number, (name, title, sky, glow, middle, front) in enumerate(scenes, 1):
        svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="900" viewBox="0 0 1200 900">
<defs><linearGradient id="sky" x2="0" y2="1"><stop stop-color="{sky}"/><stop offset="1" stop-color="{glow}"/></linearGradient></defs>
<rect width="1200" height="900" fill="url(#sky)"/>
<circle cx="{820 - number * 18}" cy="260" r="108" fill="{glow}" opacity=".85"/>
<path d="M0 640 220 360 410 560 640 290 930 620 1200 420V900H0Z" fill="{middle}"/>
<path d="M0 700 200 600 390 690 680 510 910 660 1200 540V900H0Z" fill="{front}"/>
<path d="M640 290 570 420 640 391 700 430Z" fill="{glow}" opacity=".65"/>
<text x="64" y="80" font-family="sans-serif" font-size="19" letter-spacing="6" fill="#ffffff" opacity=".7">AURORA / FIELD STUDIES</text>
<text x="64" y="792" font-family="sans-serif" font-weight="300" font-size="52" letter-spacing="3" fill="#fff1df">{title.replace('&', '&amp;')}</text>
<text x="66" y="840" font-family="sans-serif" font-size="17" letter-spacing="4" fill="#fff1df" opacity=".65">COLLECTION 01 — STUDY {number:02}</text>
</svg>'''
        source = project / 'Design' / f'{name}.svg'
        write(source, svg)
        target = project / 'Exports' / f'{name}.png'
        target.parent.mkdir(parents=True, exist_ok=True)
        sp.run(['rsvg-convert', str(source), '-o', str(target)], check=True)
    for parent, name, text in [
        ('Notes', 'ND.md', '# ND / project index\n\nA small index for the North Dock collection.\n\n## Active projects\n\n- Aurora: a collection of quiet landscapes\n- Meridian: a new editorial direction\n- Northstar: a home for field notes\n\n## People\n\nNaDya leads art direction across the studio.\n\n## Next up\n\nReview the latest colour studies and prepare the autumn handoff.\n'),
        ('Notes', 'nd-brief.md', '# North Dock / brief\n\nAn independent design studio exploring place, colour and type.\n'),
        ('Projects/Aurora/Research', 'NaDya-design-notes.md', '# NaDya / design notes\n\nKeep the palette quiet. Let the work speak.\n'),
        ('Projects/Aurora', 'NaDya-handoff.json', '{"designer":"NaDya","project":"Aurora","status":"ready for review","exports":9}\n'),
        ('Projects/Meridian', 'NaDya-brand-guide.md', '# Meridian / brand guide\n\nArt direction by NaDya.\n'),
        ('Notes', 'north-dock-agenda.md', '# North Dock\n\nWeekly studio review and planning.\n'),
        ('Deliverables', 'north-dock-handoff.md', '# Handoff\n\nNine final images and one accessible website.\n'),
        ('Projects/Northstar', 'weekend-field-notes.md', '# Field notes\n\nA weekend in the northern valleys.\n'),
    ]:
        write(studio / parent / name, text)
    documents = studio / 'Documents'
    (documents / 'Assets').mkdir(parents=True, exist_ok=True)
    write(documents / 'AGENTS.md', '# Studio guide\n\n## Working together\n\n- Keep changes focused.\n- Explain the intent.\n- Review before release.\n\n## Project structure\n\nSource lives in src/.\nArtwork lives in Assets/.\n\n## Before shipping\n\nRun the checks and review\nthe updated previews.\n')
    write(documents / 'README.md', '# Aurora Studio\n\nA small home for big ideas.\n\n## Getting started\n\n1. Explore the collection.\n2. Choose a direction.\n3. Make something useful.\n\n## This release\n\n- A quieter interface\n- More room for discovery\n- Thoughtful little details\n\nDesigned slowly.\nBuilt with care.\n')
    write(documents / 'build.py', '#!/usr/bin/env python3\nfrom pathlib import Path\n\nROOT = Path("studio")\nOUTPUT = ROOT / "dist"\n\ndef build_collection():\n    OUTPUT.mkdir(exist_ok=True)\n    for source in ROOT.glob("*.md"):\n        title = source.stem\n        print(f"Building {title}")\n\nif __name__ == "__main__":\n    build_collection()\n')
    write(documents / 'config.json', '{\n  "name": "Aurora",\n  "theme": "night",\n  "collections": [\n    "Alpine",\n    "Coastal",\n    "Desert"\n  ],\n  "gallery": {\n    "columns": 3,\n    "gap": 24\n  },\n  "preview": true\n}\n')
    write(documents / 'main.rs', 'use std::path::Path;\n\nstruct Collection {\n    name: String,\n    images: usize,\n}\n\nfn main() {\n    let studio = Collection {\n        name: "Aurora".into(),\n        images: 9,\n    };\n    println!("{}: {} images",\n        studio.name, studio.images);\n}\n')
    write(documents / 'notes.txt', 'FIELD NOTES / OCTOBER\n\nA few things worth keeping.\n\nLight across the ridge.\nSalt on the windows.\nA path through the pines.\n\nNext time, bring a notebook.\nLeave room in the margins.\n\nOne idea per page.\nOne place at a time.\n')
    write(documents / 'package.toml', '[package]\nname = "aurora"\nversion = "1.0.0"\nedition = "2024"\n\n[studio]\nowner = "NaDya"\nstatus = "ready"\n\n[features]\npreview = true\ncollections = 3\n')
    write(documents / 'theme.css', ':root {\n  --ink: #060b1e;\n  --paper: #ffcead;\n  --accent: #7d82d9;\n}\n\n.studio {\n  display: grid;\n  gap: 24px;\n  padding: 48px;\n  color: var(--paper);\n  background: var(--ink);\n}\n')
    write(documents / 'empty.txt', '')
    (documents / 'TizenTube.wgt').write_bytes(b'PK\x03\x04\x00mock binary package')
    # Stable, fictional dates; screenshot output is independent of the capture date.
    for path in studio.rglob('*'):
        os.utime(path, (1792573200, 1792573200))


class Display:
    def __init__(self, name: str):
        self.x = C.CDLL('libX11.so.6')
        self.t = C.CDLL('libXtst.so.6')
        self.x.XOpenDisplay.argtypes = [C.c_char_p]
        self.x.XOpenDisplay.restype = C.c_void_p
        self.d = self.x.XOpenDisplay(name.encode())
        if not self.d:
            raise RuntimeError('Could not open isolated X display')
        signatures = {
            'XDefaultRootWindow': ([C.c_void_p], C.c_ulong),
            'XQueryTree': ([C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong), C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)], C.c_int),
            'XFetchName': ([C.c_void_p, C.c_ulong, C.POINTER(C.c_void_p)], C.c_int),
            'XFree': ([C.c_void_p], C.c_int),
            'XResizeWindow': ([C.c_void_p, C.c_ulong, C.c_uint, C.c_uint], C.c_int),
            'XMoveWindow': ([C.c_void_p, C.c_ulong, C.c_int, C.c_int], C.c_int),
            'XMapRaised': ([C.c_void_p, C.c_ulong], C.c_int),
            'XSetInputFocus': ([C.c_void_p, C.c_ulong, C.c_int, C.c_ulong], C.c_int),
            'XStringToKeysym': ([C.c_char_p], C.c_ulong),
            'XKeysymToKeycode': ([C.c_void_p, C.c_ulong], C.c_uint),
            'XFlush': ([C.c_void_p], C.c_int),
            'XCloseDisplay': ([C.c_void_p], C.c_int),
        }
        for symbol, (args, result) in signatures.items():
            fn = getattr(self.x, symbol)
            fn.argtypes, fn.restype = args, result
        self.t.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.t.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.t.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]

    def window(self) -> int | None:
        root = self.x.XDefaultRootWindow(self.d)
        parent, result_root, count = C.c_ulong(), C.c_ulong(), C.c_uint()
        children = C.POINTER(C.c_ulong)()
        self.x.XQueryTree(self.d, root, C.byref(result_root), C.byref(parent), C.byref(children), C.byref(count))
        found = None
        for i in range(count.value):
            name = C.c_void_p()
            if self.x.XFetchName(self.d, children[i], C.byref(name)) and name.value:
                title = C.string_at(name).decode(errors='replace')
                self.x.XFree(name)
                if 'Kestrel' in title:
                    found = children[i]
        if children:
            self.x.XFree(children)
        return found

    def focus(self, window: int) -> None:
        self.x.XMoveWindow(self.d, window, 0, 0)
        self.x.XResizeWindow(self.d, window, *SIZE)
        self.x.XMapRaised(self.d, window)
        self.x.XSetInputFocus(self.d, window, 1, 0)
        self.x.XFlush(self.d)
        time.sleep(.7)

    def key(self, key: str, *modifiers: str) -> None:
        def emit(name: str, pressed: bool) -> None:
            sym = self.x.XStringToKeysym(name.encode())
            code = self.x.XKeysymToKeycode(self.d, sym)
            if not code:
                raise RuntimeError(f'No keycode for {name}')
            self.t.XTestFakeKeyEvent(self.d, code, pressed, 0)
        for modifier in modifiers:
            emit(modifier, True)
        emit(key, True)
        emit(key, False)
        for modifier in reversed(modifiers):
            emit(modifier, False)
        self.x.XFlush(self.d)
        time.sleep(.15)

    def text(self, text: str) -> None:
        shifted = {'{': 'bracketleft', '}': 'bracketright', '_': 'minus', ':': 'semicolon'}
        for char in text:
            if char.isupper():
                self.key(char.lower(), 'Shift_L')
            elif char in shifted:
                self.key(shifted[char], 'Shift_L')
            else:
                self.key({' ': 'space', '.': 'period', '-': 'minus'}.get(char, char))

    def park(self) -> None:
        self.t.XTestFakeMotionEvent(self.d, -1, SIZE[0] - 1, SIZE[1] - 1, 0)
        self.x.XFlush(self.d)

    def click(self, x: int, y: int) -> None:
        self.t.XTestFakeMotionEvent(self.d, -1, x, y, 0)
        self.t.XTestFakeButtonEvent(self.d, 1, True, 0)
        self.t.XTestFakeButtonEvent(self.d, 1, False, 0)
        self.x.XFlush(self.d)
        time.sleep(.3)


def capture(display: str, name: str) -> None:
    time.sleep(.8)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    path = OUTPUT / f'{name}.png'
    sp.run(['import', '-display', display, '-window', 'root', str(path)], check=True)
    print(f'Captured {path.relative_to(REPO)}', flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--only', choices=['overview', 'columns', 'search', 'batch-rename', 'text-thumbnails'], help='Capture a single scene')
    parser.add_argument('--binary', type=Path, help='Use an already built binary instead of building release')
    args = parser.parse_args()
    for tool in ['cargo', 'Xvfb', 'import', 'rsvg-convert']:
        if not shutil.which(tool):
            raise SystemExit(f'Missing tool: {tool}')
    if args.binary is None:
        sp.run(['cargo', 'build', '--release', '--locked'], cwd=REPO, check=True)
    binary = args.binary.resolve() if args.binary else REPO / 'target/release/kestrel'
    if not binary.is_file():
        raise SystemExit(f'Missing binary: {binary}')
    with tempfile.TemporaryDirectory(prefix='kestrel-showcase-') as temporary:
        base = Path(temporary)
        studio = base / 'Studio'
        fixtures(studio)
        # Xvfb chooses an unused display itself. No access to the real desktop is needed.
        read_fd, write_fd = os.pipe()
        server = sp.Popen(['Xvfb', '-displayfd', str(write_fd), '-screen', '0', f'{SIZE[0]}x{SIZE[1]}x24', '-nolisten', 'tcp', '-ac'],
                          pass_fds=(write_fd,), stdout=sp.DEVNULL, stderr=(base / 'xvfb.log').open('w'))
        os.close(write_fd)
        process = None
        x = None
        try:
            with os.fdopen(read_fd) as read:
                display = ':' + read.readline().strip()
            if display == ':':
                raise RuntimeError((base / 'xvfb.log').read_text())
            env = dict(os.environ, DISPLAY=display, WINIT_UNIX_BACKEND='x11',
                       XDG_STATE_HOME=str(base / 'state'), XDG_DATA_HOME=str(base / 'data'),
                       KESTREL_OMARCHY_THEME=os.environ.get('KESTREL_OMARCHY_THEME', str(Path.home() / '.local/state/omarchy/current/theme')))
            env.pop('WAYLAND_DISPLAY', None)
            x = Display(display)
            aurora = studio / 'Projects' / 'Aurora'

            def launch(panes: list[list[tuple[Path, str]]], active: int = 0, preview: bool = True, sidebar: bool = True) -> None:
                nonlocal process
                if process:
                    process.terminate()
                    process.wait(timeout=10)
                    process = None
                    time.sleep(.3)
                text = f'hidden 0\npreview {int(preview)}\nsidebar {int(sidebar)}\nsidebar_width 186\npreview_width 430\ncollapsed BOOKMARKS\ncollapsed DEVICES\nactive_pane {active}\nsplit_ratio 0.47\n'
                for pane in panes:
                    text += 'pane 0\n'
                    for path, mode in pane:
                        text += f'tab name asc {mode} {path}\n'
                write(base / 'state/kestrel/session', text)
                log = (base / 'app.log').open('w')
                process = sp.Popen([str(binary)], cwd=studio, env=env, stdout=log, stderr=log)
                time.sleep(.5)
                deadline = time.monotonic() + 15
                while not (window := x.window()):
                    if process.poll() is not None:
                        raise RuntimeError((base / 'app.log').read_text())
                    if time.monotonic() > deadline:
                        raise RuntimeError('Timed out opening Kestrel')
                    time.sleep(.1)
                x.focus(window)
                time.sleep(1)

            if args.only in (None, 'overview'):
                launch([[(aurora, 'details'), (studio / 'Deliverables', 'details')],
                        [(aurora / 'Exports', 'icons3'), (aurora / 'Design', 'details')]], active=1)
                x.park()
                capture(display, 'overview')

            if args.only in (None, 'columns'):
                launch([[(aurora, 'columns'), (studio / 'Deliverables', 'details')]], sidebar=False)
                # The active Aurora column starts on Design. Select Website, then enter src.
                for _ in range(3):
                    x.key('Down')
                x.key('Right')
                time.sleep(.4)
                x.key('Right')
                time.sleep(.6)
                x.park()
                capture(display, 'columns')

            if args.only in (None, 'search'):
                launch([[(studio, 'details'), (aurora, 'details')]], sidebar=False)
                x.key('f', 'Control_L')
                x.text('nd')
                time.sleep(1)
                x.park()
                capture(display, 'search')

            if args.only in (None, 'batch-rename'):
                launch([[(aurora / 'Exports', 'details'), (studio / 'Deliverables', 'details')]], preview=False, sidebar=True)
                x.key('a', 'Control_L')
                x.key('F2')
                time.sleep(.6)
                x.click(700, SIZE[1] // 2 - 230)
                x.key('a', 'Control_L')
                x.text('aurora-{n}{ext}')
                x.park()
                capture(display, 'batch-rename')
            if args.only in (None, 'text-thumbnails'):
                launch([[(studio / 'Documents', 'icons4'), (aurora / 'Website' / 'src', 'icons4')]])
                x.key('Home')
                for _ in range(8):
                    x.key('Right')
                x.park()
                capture(display, 'text-thumbnails')
        finally:
            if process and process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
            if x:
                x.x.XCloseDisplay(x.d)
            server.terminate()
            server.wait(timeout=10)


if __name__ == '__main__':
    main()
