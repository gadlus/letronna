# Getting started

## Requirements

- **Windows** 11, x64
- **macOS** 13 or later, Apple silicon
- **Linux** x64, glibc 2.31 or later

## Install

```powershell
iex (irm https://letronna.ai/install.ps1)        # Windows
```

```sh
curl -fsSL https://letronna.ai/install.sh | sh   # Linux
```

On macOS, open the `.dmg` from [Download](https://letronna.ai/dashboard/download) and drag Letronna into Applications.

The script downloads the latest release, then hands over to `letronna setup`. Nothing needs administrator rights. Each [release](https://github.com/gadlus/letronna/releases/latest) also carries the archives and the same scripts.

## Setup

`letronna setup` asks:

1. What to call you
2. Whether to sign in to your Letronna account (optional if you bring your own keys)
3. Which gateway and model to use
4. Whether to install the window, the terminal, or both

Run it again any time to change the answers. It also runs on first start when nothing is configured.

## First message

```sh
letronna
```

Type a message. Letronna asks before it runs a command or changes a file. `/help` lists the commands, and `/model` switches models.

## Uninstall

```sh
letronna --uninstall
```

On Windows, Apps & features also works. Uninstalling keeps your data folder (see [Data and privacy](data-and-privacy.md)).
