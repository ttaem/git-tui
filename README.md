# Git TUI - Git Branch Graph Visualizer

A terminal-based Git repository browser with git log --graph style visualization and branch filtering.

## Features

- 🌳 **Real Git Graph Display**: Shows commits exactly like `git log --oneline --graph --decorate`
- 🎯 **Branch-based Filtering**: Select specific branches to view only their commits and children
- 📊 **ASCII Graph Lines**: Authentic git graph visualization with `*`, `|`, `/`, `\` characters
- 📋 **Commit Details**: View detailed information about selected commits
- ⌨️ **Keyboard Navigation**: Fully keyboard-driven interface
- 🔍 **Branch Selection**: Focus on specific development paths

## Key Bindings

### General
- `q` or `Esc`: Quit the application
- `Tab`: Switch between branches and commits view
- `r` or `R`: Refresh repository data

### Branch View
- `↑/↓`: Navigate through branches
- `Enter`: Select branch to filter commits (shows only selected branch and its children)
- `c` or `C`: Clear filter to show all branches
- Green `●`: Currently filtered branch
- Yellow: Current HEAD branch
- Cyan: Remote branches

### Commit View
- `↑/↓`: Navigate through commits
- Selected commit details appear in the right panel

## What's Different from Standard Git Tools

Unlike `git log --graph --all`, this tool allows you to:
1. **Interactively select specific branches** - Only show commits relevant to your selected branch
2. **Focus on branch relationships** - See how a specific branch relates to its parent commits
3. **Navigate through commit history** - Easily browse commits with detailed information
4. **Filter noise** - Hide unrelated branches when focusing on specific development paths

## Example Usage

1. Start the application in any Git repository
2. Use `↑/↓` to browse available branches
3. Press `Enter` on a branch to filter commits to show only that branch's history
4. Press `Tab` to switch to commit view and browse through commits
5. Press `c` to clear the filter and see all branches again

## Installation

1. Ensure you have Rust installed
2. Clone this repository
3. Build and run:
   ```bash
   cargo build --release
   cargo run
   ```

## Use Cases

- **Feature Branch Development**: Select a feature branch to see its development history
- **Code Review**: Focus on specific branches during pull request reviews  
- **Git History Analysis**: Understand how branches diverged and merged
- **Branch Cleanup**: Identify which branches can be safely deleted
- **Learning Git**: Visualize how Git branching and merging works

## Dependencies

- `ratatui`: Terminal UI framework
- `crossterm`: Cross-platform terminal manipulation
- `git2`: Git repository access
- `anyhow`: Error handling
- `chrono`: Date/time handling

The application uses both the `git2` library for repository metadata and calls the system `git` command for authentic graph generation, ensuring you see exactly what `git log --graph` would show.
