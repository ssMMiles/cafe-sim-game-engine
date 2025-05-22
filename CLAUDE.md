# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

- **Build**: `cargo build`
- **Run**: `cargo run`
- **Check**: `cargo check`
- **Format**: `cargo fmt`
- **Lint**: `cargo clippy`

## Architecture Overview

This is a 3D cafe simulation built with Bevy 0.15.3 using a state-driven architecture:

### State Management
- **AppState**: Top-level states (`Starting` → `MainMenu` → `InGame`)
- **MenuScreen**: Menu navigation states (`None`, `Main`, `Settings`)
- **SimulationState**: Game simulation states (`Running`, `Paused`)

### Plugin Structure
- **MenuPlugin**: Handles UI screens, camera setup, and menu navigation
- **SimulationPlugin**: Manages 3D game world, player controls, and simulation logic

### Key Components
- **PlayerCamera**: 3D camera with velocity-based movement (WASD + Space/Ctrl)
- **SimulationConfig**: Resource for simulation parameters like speed

### Module Organization
- `main.rs`: App initialization and state setup
- `game.rs`: 3D simulation logic and player movement
- `menu/`: UI system with main menu and settings screens
- `freecam.rs`: Currently empty, likely for camera utilities

The game transitions from main menu (2D camera) to in-game (3D camera with free movement) using Bevy's state system.