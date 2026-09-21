//! The shell source: `run_command`, which runs a command for a skill that
//! carries scripts, inside the sandbox in [`super::sandbox`].
//!
//! Every call asks the person first and shows the exact command, unless
//! they answered Always Allow for `shell/run_command`. A conversation gets
//! one scratch folder, so a script can leave a file for the next command
//! to read; the folder goes when the conversation does.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use mailrs_ai::{BoxFuture, ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use super::sandbox::{self, Jail, Limits};
use super::{Skill, text_field, unknown};
use crate::assistant::sources::Source;
use mailrs_domain::translate::{fill, gettext};

/// A skill whose scripts may run, with its network switch.
#[derive(Debug, Clone)]
pub struct Runnable {
    pub skill: Skill,
    pub network: bool,
}

pub struct Shell {
    bwrap: PathBuf,
    skills: Vec<Runnable>,
    scratch: Scratch,
    limits: Limits,
}

impl Shell {
    pub fn new(bwrap: PathBuf, skills: Vec<Runnable>) -> Shell {
        Shell {
            bwrap,
            skills,
            scratch: Scratch::new(scratch_root()),
            limits: Limits::DEFAULT,
        }
    }

    /// A shell with its scratch folders under `root` and its own limits,
    /// for tests.
    #[cfg(test)]
    pub fn with(bwrap: PathBuf, skills: Vec<Runnable>, root: PathBuf, limits: Limits) -> Shell {
        Shell {
            bwrap,
            skills,
            scratch: Scratch::new(root),
            limits,
        }
    }

    /// The skill a call names, by its name or its id, as the skills
    /// source finds one.
    fn runnable(&self, name: &str) -> Option<&Runnable> {
        let name = name.trim();
        let mut skills = self.skills.iter();
        skills
            .clone()
            .find(|r| r.skill.name == name)
            .or_else(|| skills.find(|r| r.skill.id == name))
    }
}

/// Where each conversation's scratch folder goes.
fn scratch_root() -> PathBuf {
    gtk::glib::user_cache_dir()
        .join("penguin-mail")
        .join("skill-work")
}

/// Removes scratch folders a crash or a kill left behind. Penguin Mail runs
/// one copy at a time and no conversation outlives it, so any folder here
/// when it starts belongs to nobody.
pub fn clear_leftovers() {
    let root = scratch_root();
    if root.exists()
        && let Err(err) = std::fs::remove_dir_all(&root)
    {
        tracing::warn!(error = %err, "could not clear old skill scratch folders");
    }
}

/// One conversation's scratch folder, made on the first command and
/// removed when the conversation ends.
struct Scratch {
    root: PathBuf,
    dir: Mutex<Option<PathBuf>>,
}

impl Scratch {
    fn new(root: PathBuf) -> Scratch {
        Scratch {
            root,
            dir: Mutex::new(None),
        }
    }

    fn get(&self) -> std::io::Result<PathBuf> {
        let mut dir = self.dir.lock().map_err(|_| std::io::Error::other("lock"))?;
        if let Some(dir) = dir.as_ref() {
            return Ok(dir.clone());
        }
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let made = self.root.join(format!(
            "{}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed),
            rand::random::<u32>()
        ));
        private_dir(&made)?;
        *dir = Some(made.clone());
        Ok(made)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.get_mut().ok().and_then(Option::take) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Makes `dir` and any missing parents readable by this user alone.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

impl Source for Shell {
    fn id(&self) -> String {
        "shell".into()
    }

    fn prompt(&self) -> Option<String> {
        let names: Vec<&str> = self.skills.iter().map(|r| r.skill.name.as_str()).collect();
        Some(format!(
            "run_command runs a shell command for a skill with scripts ({}). It runs in a \
             sandbox: the skill's folder is at /skill, read-only, and /work is a scratch \
             folder that lasts for this conversation and is the working directory. The \
             sandbox cannot reach the user's mail, files or keys, and reaches the network \
             only if the user allowed it for that skill. The user approves every command, \
             so run only what the skill needs.",
            names.join(", ")
        ))
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "run_command".into(),
            description: "Run a bash command in a sandbox for one skill, such as one of its scripts. Returns the exit code and what the command printed.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill": {"type": "string", "description": "The skill's name."},
                    "command": {"type": "string", "description": "The bash command line, run in /work. The skill's files are under /skill."},
                    "stdin": {"type": "string", "description": "Text to feed the command on standard input."}
                },
                "required": ["skill", "command"]
            }),
        }]
    }

    fn ask(&self, _name: &str, input: &Value) -> Option<String> {
        // A call naming no runnable skill fails without running anything,
        // so there is nothing to ask about.
        let runnable = self.runnable(&text_field(input, "skill"))?;
        let command = text_field(input, "command");
        let network = if runnable.network {
            gettext("It can reach the internet.")
        } else {
            gettext("It cannot reach the internet.")
        };
        Some(fill(
            &gettext(
                "Run this command for the skill {skill}?\n\n{command}\n\nIt runs in a sandbox with no access to your mail or home folder. {network}",
            ),
            &[
                ("skill", &runnable.skill.name),
                ("command", &command),
                ("network", &network),
            ],
        ))
    }

    fn call(&self, _name: String, input: Value) -> BoxFuture<ToolOutcome> {
        let skill_name = text_field(&input, "skill");
        let command = text_field(&input, "command");
        let stdin = input
            .get("stdin")
            .and_then(Value::as_str)
            .map(str::to_string);
        let runnable = self.runnable(&skill_name).cloned();
        let work = self.scratch.get();
        let bwrap = self.bwrap.clone();
        let limits = self.limits;
        Box::pin(async move {
            let Some(runnable) = runnable else {
                return unknown(&skill_name);
            };
            if command.trim().is_empty() {
                return ToolOutcome::Err("Give a command to run.".into());
            }
            let work = match work {
                Ok(work) => work,
                Err(err) => return ToolOutcome::Err(format!("No scratch folder: {err}")),
            };
            let jail = Jail {
                bwrap: &bwrap,
                skill: &runnable.skill.folder,
                work: &work,
                network: runnable.network,
            };
            match sandbox::run(&jail, &command, stdin.as_deref(), limits).await {
                Ok(run) => ToolOutcome::Ok(report(&run, limits)),
                Err(err) => ToolOutcome::Err(format!("The sandbox did not start: {err}")),
            }
        })
    }
}

/// What the model reads back, with a note for anything cut short.
fn report(run: &sandbox::Run, limits: Limits) -> Value {
    let mut answer = json!({
        "exit_code": run.exit_code,
        "output": run.output,
    });
    let mut notes = Vec::new();
    if run.timed_out {
        notes.push(format!(
            "The command ran past {} seconds and was stopped.",
            limits.time.as_secs()
        ));
    }
    if run.truncated {
        notes.push(format!(
            "The output ran past {} bytes; only the start is here.",
            limits.output
        ));
    }
    if !notes.is_empty() {
        answer["note"] = json!(notes.join(" "));
    }
    answer
}
