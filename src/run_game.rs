//! Run: the open project as its own process (`cargo run -r` in its directory), its output in
//! the editor's log. Saved files reach the running game through its own asset hot reload.

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

use bevy::prelude::*;
use jackdaw_api::prelude::*;

/// The running game, so a second Run restarts it instead of starting another.
#[derive(Resource, Default)]
pub struct RunningGame(Mutex<Option<Child>>);

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<RunningGame>();
}

/// Run the project. A game already running from here is stopped first.
#[operator(
    id = "project.run",
    label = "Run",
    description = "Run the project (`cargo run -r` in its directory) as its own process.",
    allows_undo = false
)]
pub fn project_run(
    _: In<OperatorParameters>,
    project: Option<Res<crate::project::ProjectRoot>>,
    running: Res<RunningGame>,
) -> OperatorResult {
    let Some(project) = project else {
        warn!("project.run: no project is open");
        return OperatorResult::Cancelled;
    };
    let mut slot = running.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut previous) = slot.take() {
        let _ = previous.kill();
        let _ = previous.wait();
    }
    let mut child = match Command::new("cargo")
        .args(["run", "-r"])
        .current_dir(&project.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            error!("project.run: cargo run failed to start: {err}");
            return OperatorResult::Cancelled;
        }
    };
    info!("running {}", project.root.display());
    if let Some(out) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                info!(target: "game", "{line}");
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                info!(target: "game", "{line}");
            }
        });
    }
    *slot = Some(child);
    OperatorResult::Finished
}
