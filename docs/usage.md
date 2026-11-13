# Advanced usage

## Search

Type to filter a folder; Ctrl+F searches subfolders. Queries support `ext:pdf,jpg`,
`type:folder`, `type:file`, `"literal phrase"`, and `=exact-name.txt`. `type:` also
takes a kind of file: `image`, `video`, `audio` (or `music`), `document` (or `doc`),
`archive`, `code`, or `text`. The status bar flags anything that looks like a
filter but isn't one, such as `size:>1M`.

Matches show up while subfolders are still being scanned, and editing the query
mid-scan re-checks everything found so far. Later queries reuse the scan; press
F5 to pick up files added deep inside the folder since then.

## Run shell scripts

Right-click a `.sh` file and choose **Run in terminal** to run it with Bash.
The script runs in its containing folder. This requires `xdg-terminal-exec`.
Run only scripts you trust.

## Custom actions

Run your own shell commands on the selection. Open **Custom actions: add or
edit…** from the command palette, Help (F1), or any right-click Actions menu to
add, reorder and test them: the editor previews the exact command for what's
selected, and starts you off with a few examples. Saving writes
`~/.config/kestrel/actions.conf`, which you can also edit by hand (press F5 to
reload it):

```ini
[Convert to WebP]
match = *.png, *.jpg
run = magick {file} {dir}/{stem}.webp

[Edit in Neovim]
run = nvim {files}
terminal = true
```

Actions appear in the command palette and the right-click Actions menu; ones that
don't use the selection also appear when you right-click empty space. Commands run
through `sh -c`. Kestrel quotes path placeholders, so don't add your own quotes:

- `{file}`, `{name}`, `{stem}`, `{ext}`: each selected item; the command runs once
  per item, in that item's folder. A failure doesn't stop the others.
- `{files}`: all selected paths in one command.
- `{dir}`: the item's folder, or the current folder.

`match` limits an action to selections whose names all match one of its patterns.
`terminal = true` (or `yes`) runs it in a terminal via `xdg-terminal-exec`. Saving
from the editor keeps your comments; mistakes in the file are reported instead of
silently skipped. See [examples](custom_actions.md) for more ideas.

## Shell file pickers

```sh
kestrel --pick open --multiple --dir ~/Pictures --title "Choose images"
kestrel --pick folder --dir ~/Projects
kestrel --pick save --dir ~/Documents --name report.md
```

Chosen paths go to stdout, one per line; cancellation prints nothing. Picker
windows leave your regular session unchanged. The window class is `kestrel-picker`.

## Theme override

```sh
KESTREL_OMARCHY_THEME=/usr/share/omarchy/themes/gruvbox kestrel
```

## Release archive

`./scripts/package-release.sh` builds a Linux x86_64 archive and checksums under
`target/releases`. The archive includes its installer and desktop integration helper.

## Screenshots

`python3 scripts/screenshots.py` recreates README images using fictional files
and generated artwork on an isolated display. It needs Cargo, Xvfb, ImageMagick,
librsvg, and libXtst. Use `--only text-thumbnails` to refresh that scene alone.
