# Strata

A fast, multi-project code editor built for developers who work across many repositories at once.

---

Strata is a high-performance code editor with native support for **multi-folder workspaces**. Open multiple project folders in a single window, each with its own terminal, git state, and file tree — all navigable from a unified sidebar.

Built on a GPU-accelerated UI framework written in Rust, Strata delivers the responsiveness of a terminal editor with the features of a full IDE.

### Key Features

- **Multi-Folder Workspaces** — Open several project directories in one window. Each folder gets its own isolated workspace with independent git state, terminal, and editor tabs.
- **Unified Sidebar** — A single sidebar shows all your open projects at a glance with git branch names, file trees, and terminal activity indicators. Click to switch context instantly.
- **Integrated Terminals** — Every workspace auto-opens a terminal in the center pane (not a bottom dock) with the correct working directory. Terminals live alongside code files as regular tabs.
- **Fast** — GPU-rendered UI, Rust from top to bottom, instant startup. No Electron.
- **Tree-sitter & LSP** — Syntax highlighting via Tree-sitter, full language server support for completions, diagnostics, go-to-definition, and more.
- **Vim Mode** — First-class modal editing support.
- **Collaboration** — Real-time multiplayer editing with shared workspaces.
- **Extensible** — WebAssembly-based extension system for languages, themes, and tools.

### Quick Start

**From source (macOS):**

```sh
# Install dependencies
brew install cmake

# Build and run
cargo run -p zed
```

Enable multi-folder workspaces in your settings:

```json
{
  "multi_folder_workspaces": {
    "enabled": true
  }
}
```

Then use the **Open Folders as Workspaces** command to open multiple directories in a single window.

### Architecture

Strata leverages a `MultiWorkspace` architecture where each open folder is a fully isolated workspace entity with its own:

- **Project** — worktree, language servers, diagnostics
- **Center pane** — editor tabs and terminal tabs side by side
- **Git state** — independent branch tracking per folder
- **Docks** — git panel, outline panel, debug panel

The unified sidebar is rendered by the `MultiWorkspace` container, which manages workspace switching and provides at-a-glance status for all folders.

### Development

- [Building for macOS](./docs/src/development/macos.md)
- [Building for Linux](./docs/src/development/linux.md)
- [Building for Windows](./docs/src/development/windows.md)

### License

Licensed under GPL-3.0. See [LICENSE](./LICENSE-GPL) for details.

Third-party dependency licenses are managed via [`cargo-about`](https://github.com/EmbarkStudios/cargo-about).
