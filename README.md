# PaperWall

An efficient live wallpaper app.

| Alternative      | Operating System Support | Official Installation Path(s) | Pricing Model |
| ---------------- | ------------------------ | ----------------------------- | ------------- |
| Hidamari         | Linux                    | Flathub                       | Free          |
| Wallspace        | MacOS                    | Direct or Homebrew            | Freemium      |
| Wallpaper Engine | Windows                  | Humble Store, Steam           | Paid          |
| ScreenPlay       | Linux, MacOS, Windows    | Steam                         | Free          |
| PaperWall        | MacOS                    | Direct (planned)              | Free          |

## Specifications

Minimally supports MP4.

Shuns high utilisation and permanent bloat.

| Operating System | Settings Path                                           |
| ---------------- | ------------------------------------------------------- |
| MacOS            | `~/Library/Application Support/paperwall/settings.json` |
| Windows          | `%LOCALAPPDATA%\paperwall\settings.json`                |

<!-- Lowercase app spelling for paths: you are welcome, devs. -->

The settings are stored in the above path with default being:
```json
{
  "video_folder": "C:\\Users\\...\\Videos",
  "scaling_mode": "Fill Screen"
}
```

UI example:
```
Video Folder [selector]
Scaling Mode [dropdown]

[ thumbnail 1 ] [ thumbnail 2 ]
```

Video thumbnails where the existing clickable boxes are.
They use the middle frame of the video (taking the floor if dividing by 2 is non-integer).

Minimum picker window width: 800px.
Minimum picker window height: 600px.

The scaling mode dropdown has options:
- Fill Screen: centered max(H/h, W/w) with any excess cropped.
- Fit to Screen: centered min(H/h, W/w) with any deficiency filled by black bars.
