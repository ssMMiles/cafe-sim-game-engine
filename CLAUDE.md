# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Key Information

NEVER guess or make assumptions about the codebase or external dependencies. Always ask yourself if you are 100% confident in something, and if not ask for clarification or investigate further before proceeding.

## Development Commands

- **Build**: `cargo build`
- **Run**: `cargo run`
- **Check**: `cargo check`
- **Format**: `cargo fmt`
- **Lint**: `cargo clippy`

## Architecture Overview

This is a 3D cafe simulation built with Bevy 0.17.2 using a state-driven architecture:

### State Management
- **AppState**: Top-level states (`Starting` → `MainMenu` → `InGame`)
- **MenuScreen**: Menu navigation states (`None`, `Main`, `Settings`)
- **SimulationState**: Game simulation states (`Running`, `Paused`)

### Plugin Structure
- **MenuPlugin**: Handles UI screens, camera setup, and menu navigation
- **SimulationPlugin**: Manages 3D game world, player controls, and simulation logic
- **SettingsPlugin**: Centralizes user settings management

### Key Components
- **PlayerCamera**: 3D camera with velocity-based movement (WASD + Space/Ctrl)
- **SimulationConfig**: Resource for simulation parameters like speed
- **UserSettings**: Centralized settings resource for crosshair visibility and movement mode

### Key Systems
- **Mesh Raycasting**: Uses Bevy's `MeshRayCast` for precise object interaction detection
- **Settings Integration**: Real-time UI updates showing current setting values and keybinds
- **Keybinds Display**: Top-right UI showing controls (Shift+T for crosshair, Shift+M for movement mode)

### Module Organization
- `main.rs`: App initialization and state setup
- `game/`: 3D simulation logic, camera controls, and interaction systems
  - `simulation.rs`: Game world setup, interaction detection, UI systems
  - `camera.rs`: Player camera with FreeCam/FloorBased movement modes
  - `pause_menu.rs`: Pause menu functionality
- `menu/`: UI system with main menu and settings screens
- `settings.rs`: Centralized user settings store and management

The game transitions from main menu (2D camera) to in-game (3D camera with free movement) using Bevy's state system.

## Memories and Development Notes

- Whenever you make a change that affects the project structure or just something that you find might be important to remember, please write it to your memory file yourself.
- Always take your time to write clean, well designed and organised code. Do not repeat yourself, but also don't break things up to a ridiculous extent before it is needed.
- You have full access to the bevy repository (in ../bevy) if you need to check anything about bevy itself.
- Instead of running the program as it's a visual app you can't interact with, just ask me. You can however run tests and check if things compile.