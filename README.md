# Stable

A keyboard-driven TUI dashboard to orchestrate CLI agents for 10x engineers.

Install Stable to keep your trusty steed's harness under the solid roof!

# Behold

## Dashboard view

![](/docs/demo/dashboard-view.png)

## Agent view

OpenCode

![](/docs/demo/agent-view-opencode.png)

Claude Code

![](/docs/demo/agent-view-claude-code.png)

# What

- Opinionated agent manager done my way, because I couldn't find one that's built the way I need.
- Not laser-focused on software development only!
- Pure grid layout, no left panel bullshit!
- Keyboard-driven navigation and interaction with sane amount of mouse support.
- Auto-detection of installed agent CLIs, hooks are installed on first run.
- Focus on quick navigation through active agent sessions and history.
- Survives tmux restarts.
- Single binary, no stupid js runtimes!

# Plan

- [ ] improve agent status detection
- [ ] quick switching through: running, waiting (idle), last responded agents.
- [ ] split-screen mode to watch several running agents.
- [ ] git awarness: branch names, worktrees, diff views.
- [ ] filtering (with fuzzysearch): by name, agent type, working directory, etc.
- [ ] search in agent sessions history

# Tech considerations

- Built in Rust :heart: btw!
- Consumes around 100MB of memory and does not burn your CPU!
- Depends on tmux, so you must install it!
- The code is garbage because I vibe coded it!
- Supported harnesses: OpenCode, Claude Code (pi is the next one?)
