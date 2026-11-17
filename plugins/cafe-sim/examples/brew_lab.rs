//! The cafe's drink minigames and their overlay, standing on their own.
//!
//! ```text
//! # play one minigame in a window
//! tools/brew-lab.sh --game froth
//! # play a whole recipe
//! tools/brew-lab.sh --drink Latte
//!
//! # run it headless off a script and write PNGs (no window, fixed timestep)
//! tools/brew-lab.sh --game trace --script heart.lab --shots out/ --shot-every 10
//! ```
//!
//! `tools/brew-lab.sh` builds it through the workspace so bevy keeps the
//! feature set the engine resolves — building this package alone resolves
//! fewer and rebuilds bevy into target/ from scratch.
//!
//! The harness itself is `cafe_sim::lab`; this file is the command line.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowResolution};
use bevy::winit::WinitPlugin;
use cafe_sim::lab::{
    BrewLabPlugin, GAMES, LabConfig, LabScript, drink_named, drink_names, game_named,
};

const USAGE: &str = "\
brew-lab - run the cafe's drink minigames on their own

  --game <name>        play one minigame (see below)
  --drink <name>       play a whole recipe
  --script <file>      drive it from a script; implies headless
  --shots <dir>        write PNG screenshots there (headless only)
  --shot-every <n>     screenshot every n frames
  --exit-after <n>     give up after n frames
  --size <w>x<h>       target size (default 1280x720)
  --dt <secs>          fixed timestep when scripted (default 1/60)
  -h, --help

Script format:
  10: cursor 0.5 0.25          # 0..1 across the cursor surface, y up
  40: cursor_to 0.5 0.75 over 30
  5:  press 12                 # held 12 frames; `press` alone stays down
  70: release
  75: point confirm            # aim at the Done button, then `press` to click
  80: key enter                # or `space`
  90: shot settled             # -> settled.png
  99: quit
";

fn main() -> ExitCode {
    match parse() {
        Ok(Some(config)) => {
            run(config);
            ExitCode::SUCCESS
        }
        Ok(None) => {
            print!("{USAGE}");
            println!("Minigames: {}", GAMES.join(", "));
            println!("Drinks:    {}", drink_names().join(", "));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("brew-lab: {message}");
            eprintln!("try --help");
            ExitCode::FAILURE
        }
    }
}

/// `Ok(None)` means "usage was asked for".
fn parse() -> Result<Option<LabConfig>, String> {
    let mut config = LabConfig::default();
    let mut script_path: Option<PathBuf> = None;
    let mut chose_recipe = false;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--game" => {
                let name = value()?;
                config.drink =
                    game_named(&name).ok_or_else(|| format!("no minigame called `{name}`"))?;
                chose_recipe = true;
            }
            "--drink" => {
                let name = value()?;
                config.drink =
                    drink_named(&name).ok_or_else(|| format!("no drink called `{name}`"))?;
                chose_recipe = true;
            }
            "--script" => script_path = Some(PathBuf::from(value()?)),
            "--shots" => config.shot_dir = Some(PathBuf::from(value()?)),
            "--shot-every" => config.shot_every = number(&value()?)?,
            "--exit-after" => config.exit_after = number(&value()?)?,
            "--dt" => {
                let secs = value()?;
                config.fixed_dt = Some(
                    secs.parse()
                        .map_err(|_| format!("`{secs}` is not a number of seconds"))?,
                );
            }
            "--size" => {
                let size = value()?;
                let (w, h) = size
                    .split_once('x')
                    .ok_or_else(|| format!("`{size}` is not <width>x<height>"))?;
                config.size = UVec2::new(number(w)?, number(h)?);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    if !chose_recipe {
        return Err("pick a --game or a --drink".into());
    }
    if let Some(path) = script_path {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        config.script =
            Some(LabScript::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?);
        config.fixed_dt = config.fixed_dt.or(Some(1.0 / 60.0));
    }
    if let Some(dir) = config.shot_dir.as_ref() {
        if config.script.is_none() {
            return Err("--shots needs --script: screenshots come from the headless target".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    Ok(Some(config))
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("`{text}` is not a number"))
}

fn run(config: LabConfig) {
    let mut app = App::new();
    if config.headless() {
        // No window at all: the camera renders to an image and screenshots
        // copy from that, so nothing depends on a surface being on screen (a
        // covered window stops presenting on macOS) or steals focus.
        app.add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                    ..default()
                })
                .disable::<WinitPlugin>(),
        )
        .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::ZERO));
    } else {
        app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("brew lab - {}", config.drink.name),
                resolution: WindowResolution::new(config.size.x, config.size.y),
                ..default()
            }),
            ..default()
        }));
    }
    app.add_plugins(BrewLabPlugin(config)).run();
}
