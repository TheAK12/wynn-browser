# Wynn Browser

A modern, privacy-focused web browser for Linux built with Rust, GTK4, libadwaita, and WebKitGTK 6.0. Designed around a sidebar-first layout with vertical tabs, workspaces, and a command palette.

![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)
![Rust](https://img.shields.io/badge/rust-edition%202021-orange)
![GTK](https://img.shields.io/badge/GTK-4.12%2B-green)
![Platform](https://img.shields.io/badge/platform-Linux-lightgrey)

---

## Table of Contents

- [Screenshots](#screenshots)
- [Features](#features)
- [System Requirements](#system-requirements)
- [Installing Dependencies](#installing-dependencies)
- [Building from Source](#building-from-source)
- [Running](#running)
- [Keyboard Shortcuts](#keyboard-shortcuts)
- [Command Palette](#command-palette)
- [Password Manager](#password-manager)
- [Ad Blocker](#ad-blocker)
- [Privacy Features](#privacy-features)
- [Project Structure](#project-structure)
- [Contributing](#contributing)
- [License](#license)

---

## Screenshots
<img width="1920" height="1080" alt="image" src="https://github.com/user-attachments/assets/59e793eb-73e7-4275-b33b-6c65489782e2" />
<img width="1920" height="1075" alt="image" src="https://github.com/user-attachments/assets/c32399f8-7dfb-481c-9d6e-d6e7755600fb" />
<img width="1919" height="1079" alt="image" src="https://github.com/user-attachments/assets/f57811ee-58d7-45b1-aad3-3e34ff9e94ef" />
<img width="1919" height="1079" alt="image" src="https://github.com/user-attachments/assets/fe7a88ec-fd4f-4afe-ba67-a5adefa88547" />


---

## Features

### Sidebar-First Design
- Vertical tab sidebar with drag-to-resize (default 280px, min 220px)
- macOS-style traffic light buttons (close/minimize/maximize) rendered with Cairo
- All browser controls live in the sidebar: URL bar, navigation buttons, menu
- Clean content area with just the WebView and a thin progress bar

### Tab Management
- Vertical tab list in the sidebar with favicons, titles, and close buttons
- Tab switching, reordering, and keyboard navigation
- New tab opens with your configured homepage
- Tabs are grouped by workspace

### Workspaces
- Create, rename, and delete named workspaces
- Assign colored labels to workspaces for visual organization
- Filter the tab list by workspace or view all tabs at once
- New tabs are automatically assigned to the active workspace

### Split View
- Side-by-side dual browser panes in a resizable horizontal split
- Each pane has its own independent WebView, URL, and navigation state
- Toggle on/off with a single shortcut

### Command Palette
- VS Code-style overlay (Ctrl+Shift+P) with translucent backdrop
- 29 searchable commands across 5 categories
- Fuzzy matching with smart scoring (exact > prefix > word boundary > substring > fuzzy)
- Recently used commands float to the top
- Full keyboard navigation (arrow keys, Enter, Escape)

### Bookmarks
- One-click bookmark toggle from the URL bar (star icon)
- Bookmarks dialog with full list and navigation
- Stored in SQLite

### History
- Automatic visit recording with timestamps and visit counts
- Searchable history dialog
- Clear all history option

### Downloads
- WebKit-native download handling with progress tracking
- Downloads dialog showing status, file names, and progress
- Notification toasts on download completion

### Settings
- Multi-page preferences window with 4 sections:
  - **General**: Homepage, search engine, sidebar position
  - **Privacy**: ITP, DNT, custom user agent, WebRTC leak prevention, cookie policy
  - **Content**: Ad blocker toggle
  - **Appearance**: UI customization

### Developer Tools
- Built-in WebKit Web Inspector (F12)
- View page source
- Print page support

---

## System Requirements

| Requirement | Minimum Version |
|-------------|----------------|
| **Rust** | 1.70+ (stable) |
| **GTK 4** | 4.12+ |
| **libadwaita** | 1.5+ |
| **WebKitGTK 6.0** | 2.42+ |
| **GLib / GIO** | 2.72+ |
| **C compiler** | gcc or clang |
| **pkg-config** | any recent version |

SQLite is bundled and compiled from source automatically (no system `libsqlite3` needed).

---

## Installing Dependencies

### Arch Linux

```bash
sudo pacman -S --needed \
    rust \
    gtk4 \
    libadwaita \
    webkitgtk-6.0 \
    base-devel \
    pkgconf
```

### Ubuntu / Debian (24.04+ required)

Ubuntu 22.04 ships GTK 4.6 and libadwaita 1.1, which are too old. You need Ubuntu 24.04 (Noble) or later.

```bash
sudo apt install \
    rustc cargo \
    libgtk-4-dev \
    libadwaita-1-dev \
    libwebkitgtk-6.0-dev \
    libsoup-3.0-dev \
    libjavascriptcoregtk-6.0-dev \
    libglib2.0-dev \
    build-essential \
    pkg-config
```

### Fedora (40+)

```bash
sudo dnf install \
    rust cargo \
    gtk4-devel \
    libadwaita-devel \
    webkitgtk6.0-devel \
    libsoup3-devel \
    javascriptcoregtk6.0-devel \
    glib2-devel \
    gcc \
    pkg-config
```

---

## Building from Source

```bash
git clone https://github.com/TheAK12/wynn-browser.git
cd wynn-browser
cargo build --release
```

The first build will take a few minutes as it compiles all dependencies (including SQLite from source). Subsequent builds are incremental and much faster.

The compiled binary will be at `target/release/wynn-browser`.

---

## Running

```bash
./target/release/wynn-browser
```

Or directly after building:

```bash
cargo run --release
```

Data is stored at `~/.local/share/wynn-browser/wynn.db` (following the XDG Base Directory Specification).

---

## Keyboard Shortcuts

### Navigation

| Shortcut | Action |
|----------|--------|
| `Ctrl+L` | Focus the address bar |
| `Alt+Left` | Go back |
| `Alt+Right` | Go forward |
| `Alt+Home` | Go to homepage |
| `F5` / `Ctrl+R` | Reload page |
| `Ctrl+Shift+C` | Copy current URL to clipboard |

### Tabs

| Shortcut | Action |
|----------|--------|
| `Ctrl+T` | New tab |
| `Ctrl+W` | Close tab |
| `Ctrl+Tab` | Next tab |
| `Ctrl+Shift+Tab` | Previous tab |

### View

| Shortcut | Action |
|----------|--------|
| `Ctrl+\` | Toggle sidebar |
| `Ctrl+Shift+E` | Toggle split view |
| `F11` | Toggle fullscreen |
| `Ctrl+=` / `Ctrl++` | Zoom in |
| `Ctrl+-` | Zoom out |
| `Ctrl+0` | Reset zoom to 100% |

### Tools

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+P` | Open command palette |
| `Ctrl+D` | Toggle bookmark |
| `Ctrl+H` | Show history |
| `Ctrl+J` | Show downloads |
| `Ctrl+,` | Open settings |
| `Ctrl+P` | Print page |
| `F12` | Open developer tools |

---

## Command Palette

Press `Ctrl+Shift+P` to open the command palette. It provides quick access to every browser action through a searchable, keyboard-navigable overlay.

**29 commands** organized into 5 categories:

| Category | Commands |
|----------|----------|
| **Navigation** | Go Home, Go Back, Go Forward, Reload Page, Stop Loading, Focus Address Bar, Copy URL |
| **Tabs** | New Tab, Close Tab, Next Tab, Previous Tab |
| **View** | Toggle Sidebar, Toggle Split View, Toggle Fullscreen, Zoom In, Zoom Out, Reset Zoom |
| **Privacy** | Toggle Ad Blocker, Refresh Ad Blocklist, Clear History |
| **Tools** | Toggle Bookmark, Show History, Show Bookmarks, Show Downloads, Show Passwords, Settings, Print Page, View Source, Open Developer Tools |

The palette uses a multi-tier scoring system for search results:
1. Exact match
2. Prefix match
3. Word boundary match
4. Substring match
5. Fuzzy character match

Recently used commands are ranked higher.

---

## Password Manager

Wynn includes a built-in password manager with proper cryptographic security.

### How It Works

- **Key Derivation**: Your master password is processed through Argon2id (m=19456 KiB, t=2, p=1) with a random 16-byte salt to produce a 256-bit encryption key.
- **Encryption**: Each saved password is encrypted with AES-256-GCM using a unique random 96-bit nonce. The stored format is `base64(nonce || ciphertext || tag)`.
- **Verification**: A known string ("wynn-pw-ok") is encrypted with your key and stored. When you enter your master password on subsequent launches, the browser derives the key and attempts to decrypt this token. If it succeeds, the password is correct.
- **Session Key**: The derived key is held in memory only (thread-local storage) for the duration of your session. It is never written to disk.

### Usage

1. The first time you access any password feature, you will be prompted to set a master password (minimum 6 characters).
2. On subsequent sessions, you will be asked to unlock with your master password.
3. Once unlocked, the browser can:
   - Detect login form submissions and offer to save credentials
   - Auto-fill saved credentials on recognized sites
   - Manage saved passwords through the password dialog (via the menu or command palette)

### Important

Your master password cannot be recovered. If you forget it, saved passwords are permanently inaccessible.

---

## Ad Blocker

Wynn ships with a three-layer ad blocking system:

### Layer 1: Domain Blocking
Fetches and maintains a blocklist of known ad/tracking domains (Pete Lowe's blocklist). Blocked domains are checked on every page load via injected JavaScript that overrides `fetch()` and `XMLHttpRequest` to intercept requests to blocked hosts.

### Layer 2: CSS Cosmetic Filters
Hides common ad containers, banners, overlays, and cookie consent popups using CSS injection. Includes specific rules for YouTube and Spotify ad elements.

### Layer 3: Native WebKit Content Filter
Compiles blocking rules into WebKit's native content filter format (the same JSON format used by Safari content blockers). This operates at the network level before resources are downloaded.

### YouTube Ad Skip
Detects the `.ad-showing` class on YouTube's video player and automatically:
- Mutes the ad audio
- Sets playback speed to 16x
- Clicks the skip button when available
- Polls every 500ms until the ad is gone
- Restores normal playback afterward

The ad blocker can be toggled on/off from the command palette or settings. The blocklist can be refreshed on demand.

---

## Privacy Features

| Feature | Description |
|---------|-------------|
| **Intelligent Tracking Prevention** | WebKit's built-in ITP, enabled by default |
| **Do Not Track / Global Privacy Control** | DNT and GPC headers injected via JavaScript on every page load |
| **Custom User Agent** | Configurable UA string (defaults to a Firefox UA to reduce fingerprinting) |
| **WebRTC Leak Prevention** | Forces relay-only ICE candidates to prevent IP address leaks through WebRTC |
| **Third-Party Cookie Blocking** | Configurable cookie acceptance policy |
| **Ephemeral Web Storage** | Option for ephemeral browsing sessions |

All privacy features are configurable through the Settings window (Privacy tab).

---

## Project Structure

```
wynn-browser/
├── Cargo.toml             # Project manifest and dependencies
├── src/
│   ├── main.rs            # Application entry point
│   ├── window.rs          # Main window, sidebar, tabs, workspaces, actions (1645 lines)
│   ├── browser_tab.rs     # Per-tab WebKit signals, permissions, password detection (817 lines)
│   ├── webview.rs         # WebView factory, privacy hardening, URI normalization (248 lines)
│   ├── adblocker.rs       # Three-layer ad blocker, YouTube skip, content filters (835 lines)
│   ├── command_palette.rs # Command palette overlay with fuzzy search (845 lines)
│   ├── passwords.rs       # Encrypted password manager (Argon2id + AES-256-GCM) (939 lines)
│   ├── bookmarks.rs       # Bookmark CRUD and dialog (200 lines)
│   ├── history.rs         # Visit recording, search, and dialog (252 lines)
│   ├── downloads.rs       # Download handling and dialog (220 lines)
│   ├── settings.rs        # Preferences window (4 pages) (403 lines)
│   ├── database.rs        # SQLite schema, migrations, connection management (139 lines)
│   └── style.css          # UI styling (677 lines)
└── target/                # Build output (gitignored)
```

### Database Schema

The SQLite database (`~/.local/share/wynn-browser/wynn.db`) contains 5 tables:

| Table | Purpose |
|-------|---------|
| `history` | Browsing history (URL, title, visit count, timestamps) |
| `bookmarks` | Saved bookmarks (URL, title, timestamp) |
| `downloads` | Download records (URL, filename, path, size, status) |
| `passwords` | Encrypted credentials (origin, username, encrypted password, usage stats) |
| `settings` | Key-value configuration store |

---

## Rust Crate Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `gtk4` | 0.10 (v4_12) | GTK 4 widget toolkit bindings |
| `libadwaita` | 0.8 (v1_5) | GNOME HIG / Adwaita helpers |
| `webkit6` | 0.5 (v2_42) | WebKitGTK 6.0 web engine bindings |
| `glib` | 0.21 | GLib utilities, clone! macro, callbacks |
| `gio` | 0.21 | Application model, actions, I/O |
| `rusqlite` | 0.32 (bundled) | SQLite database (compiled from source) |
| `argon2` | 0.5 | Argon2id key derivation for password manager |
| `aes-gcm` | 0.10 | AES-256-GCM authenticated encryption |
| `rand` | 0.8 | Cryptographic random number generation |

---

## Contributing

Contributions are welcome. Here is how to get started:

1. Fork the repository
2. Create a feature branch (`git checkout -b my-feature`)
3. Make your changes
4. Build and test locally (`cargo build --release`)
5. Commit your changes (`git commit -m "Add my feature"`)
6. Push to your fork (`git push origin my-feature`)
7. Open a pull request

### Build Notes

- Always build in release mode (`cargo build --release`) for usable performance. Debug builds of WebKitGTK are very slow.
- If Cargo does not detect your source changes (shows `Finished in 0.0Xs` without recompiling), touch the modified files first: `touch src/file.rs && cargo build --release`.

### Code Style

- Follow standard Rust formatting (`cargo fmt`)
- Use `cargo clippy` to catch common issues
- Keep modules focused and self-contained
- Use `glib::clone!` for GTK signal closures where possible

---

## License

This project is licensed under the **GNU General Public License v3.0 or later** (GPL-3.0-or-later).

See [LICENSE](LICENSE) for the full license text.
