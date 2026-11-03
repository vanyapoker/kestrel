# Custom action examples

Ready-made actions to copy into the editor (**Custom actions: add or edit…** in the
command palette) or straight into `~/.config/kestrel/actions.conf`. See
[advanced usage](usage.md#custom-actions) for how placeholders, `match` and
`terminal` work.

## Shrink videos for sharing

Runs once per selected video, so selecting ten clips makes ten smaller copies next
to them. The terminal shows ffmpeg's progress, and one failed clip doesn't stop
the rest.

```ini
[Shrink video]
match = *.mp4, *.mov, *.mkv
run = ffmpeg -i {file} -vcodec libx264 -crf 28 -preset fast {dir}/{stem}-small.mp4
terminal = true
```

Raise `-crf` for smaller files, lower it for better quality.

## Merge PDFs, or turn images into one PDF

`{files}` hands the whole selection to a single command instead of running it once
per item.

```ini
[Merge PDFs]
match = *.pdf
run = pdfunite {files} {dir}/merged.pdf && notify-send "Kestrel" "Created merged.pdf"

[Images to one PDF]
match = *.png, *.jpg, *.jpeg
run = magick {files} {dir}/combined.pdf
```

Actions without a terminal only report failures, so `&& notify-send …` is an easy
way to confirm one worked. Both overwrite an existing `merged.pdf` or
`combined.pdf`.

## Copy text from a screenshot

Reads the text in an image (an error message, code, an address) and puts it on
the clipboard. Needs `tesseract`; add `-l deu` or similar for other installed
languages.

```ini
[Copy text from image]
match = *.png, *.jpg, *.jpeg, *.webp
run = tesseract {file} - | wl-copy && notify-send "Kestrel" "Text copied"
```

## Remove photo metadata before sharing

Phone photos carry GPS coordinates and camera details. This writes a clean copy
next to each original and leaves the original alone.

```ini
[Remove photo metadata]
match = *.jpg, *.jpeg, *.png, *.webp, *.heic
run = magick {file} -strip {dir}/{stem}-clean.{ext}
```

## Encrypt and decrypt with a password

The terminal gives gpg somewhere to ask for the passphrase. Encrypting
`report.pdf` creates `report.pdf.gpg` beside it and keeps the original; decrypting
that brings back `report.pdf`.

```ini
[Encrypt with password]
run = gpg --symmetric {file}
terminal = true

[Decrypt]
match = *.gpg
run = gpg --output {dir}/{stem} --decrypt {file}
terminal = true
```

## See a file's git history

Every change to one file, newest first, following it across renames. `LESS=R`
keeps the pager open even when the history fits on one screen, and outside a git
repository the error stays up until you press Enter.

```ini
[File history]
run = LESS=R git log -p --follow -- {file} || { echo; echo "Press Enter to close"; read _; }
terminal = true
```

## Tools for the current folder

Actions that don't use the selection also appear when you right-click empty space,
and run in the folder you're viewing.

```ini
[Lazygit here]
run = lazygit
terminal = true

[Share this folder on the LAN]
run = python3 -m http.server 8000
terminal = true

[Today's notes]
run = nvim {dir}/$(date +%F).md
terminal = true
```

Placeholders mix with ordinary shell: `$(date +%F)` names the note after today's
date. The file server is reachable by anyone on your network until you close its
terminal.
