# Advanced usage

## Search

Type to filter a folder; Ctrl+F searches subfolders. Queries support `ext:pdf,jpg`,
`type:folder`, `type:file`, `"literal phrase"`, and `=exact-name.txt`.

## Custom actions

Add commands to `~/.config/kestrel/actions.conf`, then press F5 to reload:

```ini
[Convert to WebP]
match = *.png, *.jpg
run = magick {file} {dir}/{stem}.webp

[Edit in Neovim]
run = nvim {files}
terminal = true
```

Actions appear in the command palette and context menu. Commands run through
`sh -c` in the current folder. Kestrel quotes path placeholders: `{file}`,
`{files}`, `{dir}`, `{name}`, `{stem}`, and `{ext}`; don't add your own quotes.
`terminal = true` uses `xdg-terminal-exec`.

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
