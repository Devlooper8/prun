# Icons

This folder is intentionally empty in the scaffold. Generate the full icon set
(including .icns / .ico) from a single 1024×1024 PNG with:

```
npm run tauri icon path/to/source.png
```

Tauri writes 32x32.png, 128x128.png, 128x128@2x.png, icon.icns and icon.ico
here, which is what `tauri.conf.json` references.
