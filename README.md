# Glacia

A terminal emulator built to fit my taste, available on Windows, macOS, and Linux.

## Install

Grab a pre-built binary from [Releases](../../releases/latest) or build from source:

```sh
cargo build --release
```

The binary lands at `target/release/glacia`.

## Build dependencies

**Linux** — requires the following system libraries:

```sh
sudo apt-get install libvulkan-dev libxcb-xfixes0-dev libxcb-shape0-dev \
  libxkbcommon-dev pkg-config libegl-mesa0 libgtk-3-dev
```

**macOS / Windows** — no extra dependencies.

## Configuration

Glacia reads its config from the standard config directory on each platform:

| Platform | Path |
|----------|------|
| Linux    | `~/.config/glacia/config.toml` |
| macOS    | `~/.config/glacia/config.toml` |
| Windows  | `%APPDATA%\glacia\config.toml` |

## License

See [LICENSE](LICENSE).
