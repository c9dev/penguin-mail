//! Skills: folders of instructions the assistant follows for one kind of
//! task, and the sandboxed shell their scripts run in.
//!
//! A skill is a folder holding a `SKILL.md` whose front matter gives a name
//! and a description. Penguin Mail reads them from its own folder and from
//! Claude Code's, so skills the person already wrote for Claude Code work
//! here too. Each one is off until the person turns it on. The skills
//! source lists the enabled ones in the system prompt and hands the model a
//! skill's text and files when it asks; the shell source in [`shell`] runs
//! a skill's scripts under bubblewrap.

mod front_matter;
pub mod sandbox;
pub mod shell;

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use mailrs_ai::{BoxFuture, ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use super::Source;
use crate::settings::Settings;
use mailrs_domain::translate::gettext;

/// The most of one file the model gets, SKILL.md included.
pub const FILE_CAP: u64 = 256 * 1024;

/// How far the file listing walks below a skill's folder, and how many
/// files it names, so a skill that holds a whole project cannot flood the
/// model.
const LIST_DEPTH: usize = 5;
const LIST_CAP: usize = 300;

/// Extensions that mark a file as a script even without the executable bit.
const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh", "bash", "py", "js", "mjs", "cjs", "ts", "rb", "pl", "php", "lua",
];

/// Where a skill was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    PenguinMail,
    ClaudeCode,
}

impl Origin {
    /// The first half of a skill's id, stable across languages.
    pub fn key(self) -> &'static str {
        match self {
            Origin::PenguinMail => "penguin-mail",
            Origin::ClaudeCode => "claude-code",
        }
    }

    pub fn label(self) -> String {
        match self {
            Origin::PenguinMail => gettext("Penguin Mail"),
            Origin::ClaudeCode => gettext("Claude Code"),
        }
    }
}

/// One skill found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The origin and the folder's name, such as `claude-code/pdf`. The
    /// settings keep each skill's switches under it.
    pub id: String,
    pub origin: Origin,
    pub name: String,
    pub description: String,
    /// The folder with every symlink resolved, which is what the sandbox
    /// mounts and what file reads are confined to.
    pub folder: PathBuf,
    /// Whether anything beside SKILL.md looks like a program to run.
    pub has_scripts: bool,
}

/// Penguin Mail's own skills folder, which Open Folder creates.
pub fn own_folder() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("penguin-mail")
        .join("skills")
}

/// The folders skills are read from, Penguin Mail's first.
pub fn roots() -> Vec<(Origin, PathBuf)> {
    vec![
        (Origin::PenguinMail, own_folder()),
        (
            Origin::ClaudeCode,
            gtk::glib::home_dir().join(".claude").join("skills"),
        ),
    ]
}

/// Every readable skill under `roots`, sorted by name within each root. A
/// folder with a broken SKILL.md is skipped and the log says why.
pub fn discover(roots: &[(Origin, PathBuf)]) -> Vec<Skill> {
    let mut skills = Vec::new();
    for (origin, root) in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<Skill> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                // Following the link is the point: Claude Code's skills
                // folder is often a set of links into another checkout.
                if !path.is_dir() {
                    return None;
                }
                match load(*origin, &path) {
                    Ok(skill) => Some(skill),
                    Err(reason) => {
                        if path.join("SKILL.md").exists() {
                            tracing::warn!(folder = %path.display(), %reason, "skipping skill");
                        }
                        None
                    }
                }
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        skills.extend(found);
    }
    skills
}

fn load(origin: Origin, path: &Path) -> Result<Skill, String> {
    let folder = path.canonicalize().map_err(|err| err.to_string())?;
    let text = read_capped(&folder.join("SKILL.md"))?;
    let front = front_matter::parse(&text.text)?;
    let field = |key: &str| {
        front
            .fields
            .get(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("the front matter has no {key}"))
    };
    let name = field("name")?;
    let description = field("description")?;
    let folder_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_scripts = listing(&folder).iter().any(|file| file.script);
    Ok(Skill {
        id: format!("{}/{folder_name}", origin.key()),
        origin,
        name,
        description,
        folder,
        has_scripts,
    })
}

/// One file under a skill's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the skill's folder, with `/` between parts.
    pub path: String,
    pub script: bool,
}

/// The files in a skill's folder other than SKILL.md, sorted. Hidden files
/// are left out, and so are folders reached through a link, which could
/// lead anywhere or back into the skill itself.
pub fn listing(folder: &Path) -> Vec<Entry> {
    let mut files = Vec::new();
    walk(folder, "", 0, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn walk(dir: &Path, prefix: &str, depth: usize, files: &mut Vec<Entry>) {
    if depth > LIST_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= LIST_CAP {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || (depth == 0 && name == "SKILL.md") {
            continue;
        }
        let path = format!("{prefix}{name}");
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), &format!("{path}/"), depth + 1, files);
            continue;
        }
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files.push(Entry {
            script: is_script(&path, &meta),
            path,
        });
    }
}

fn is_script(path: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = meta.permissions().mode() & 0o111 != 0;
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SCRIPT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
    executable || extension || path.starts_with("scripts/")
}

/// A file's text, cut at [`FILE_CAP`].
#[derive(Debug)]
pub struct Text {
    pub text: String,
    pub cut: bool,
}

fn read_capped(path: &Path) -> Result<Text, String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.take(FILE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    let cut = bytes.len() as u64 > FILE_CAP;
    bytes.truncate(FILE_CAP as usize);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // A cut can land inside a character; only a file that is not
        // text at all fails here.
        Err(err) if cut && err.utf8_error().error_len().is_none() => {
            let valid = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => return Err("the file is not text".into()),
    };
    Ok(Text { text, cut })
}

/// Reads `path` inside a skill's `folder`, refusing anything that would
/// leave it: a `..`, an absolute path, or a link pointing out.
pub fn read_file(folder: &Path, path: &str) -> Result<Text, String> {
    let relative = Path::new(path);
    if path.trim().is_empty() {
        return Err("Give a path inside the skill's folder.".into());
    }
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(
            "The path must stay inside the skill's folder, without .. or a leading /.".into(),
        );
    }
    let root = folder.canonicalize().map_err(|err| err.to_string())?;
    let target = root
        .join(relative)
        .canonicalize()
        .map_err(|_| format!("The skill has no file {path}."))?;
    // Resolving every link first means a link inside the skill that points
    // at ~/.ssh ends up outside `root` and is caught here.
    if !target.starts_with(&root) {
        return Err(format!("{path} leads outside the skill's folder."));
    }
    if !target.is_file() {
        return Err(format!("{path} is not a file."));
    }
    read_capped(&target)
}

/// The skills source: the enabled skills, offered to the model by name.
pub struct Skills {
    skills: Vec<Skill>,
}

impl Skills {
    pub fn new(skills: Vec<Skill>) -> Skills {
        Skills { skills }
    }

    fn named(&self, name: &str) -> Option<&Skill> {
        find(&self.skills, name)
    }
}

/// The skill a tool call names, by its name or its id.
fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let name = name.trim();
    skills
        .iter()
        .find(|skill| skill.name == name)
        .or_else(|| skills.iter().find(|skill| skill.id == name))
}

fn unknown(name: &str) -> ToolOutcome {
    ToolOutcome::Err(format!(
        "There is no enabled skill called {name}. Use a name from the list in your instructions."
    ))
}

fn text_field(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Source for Skills {
    fn id(&self) -> String {
        "skills".into()
    }

    fn prompt(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut text = String::from(
            "Skills: the user installed instructions for some kinds of task. When a request \
             fits one of the skills below, call use_skill with its name before you start, \
             and follow what it says. read_skill_file reads the files it mentions.",
        );
        for skill in &self.skills {
            text.push_str(&format!("\n- {}: {}", skill.name, skill.description));
        }
        Some(text)
    }

    fn specs(&self) -> Vec<ToolSpec> {
        if self.skills.is_empty() {
            return Vec::new();
        }
        vec![
            ToolSpec {
                name: "use_skill".into(),
                description: "Read a skill's instructions and the list of files beside them."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "The skill's name, from the list in your instructions."}
                    },
                    "required": ["name"]
                }),
            },
            ToolSpec {
                name: "read_skill_file".into(),
                description: "Read one file from a skill's folder, such as a reference the skill's instructions mention.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "The skill's name."},
                        "path": {"type": "string", "description": "The file's path inside the skill's folder, as use_skill listed it."}
                    },
                    "required": ["name", "path"]
                }),
            },
        ]
    }

    fn ask(&self, _name: &str, _input: &Value) -> Option<String> {
        // Both tools only read files the person chose to turn on.
        None
    }

    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome> {
        let skill_name = text_field(&input, "name");
        let skill = self.named(&skill_name).cloned();
        let path = text_field(&input, "path");
        Box::pin(async move {
            let Some(skill) = skill else {
                return unknown(&skill_name);
            };
            let read = tokio::task::spawn_blocking(move || match name.as_str() {
                "use_skill" => use_skill(&skill),
                "read_skill_file" => read_file(&skill.folder, &path).map(|text| {
                    json!({
                        "path": path,
                        "text": text.text,
                        "cut_short": text.cut,
                    })
                }),
                other => Err(format!("The skills source has no tool called {other}.")),
            })
            .await;
            match read {
                Ok(Ok(value)) => ToolOutcome::Ok(value),
                Ok(Err(problem)) => ToolOutcome::Err(problem),
                Err(err) => ToolOutcome::Err(err.to_string()),
            }
        })
    }
}

fn use_skill(skill: &Skill) -> Result<Value, String> {
    let text = read_capped(&skill.folder.join("SKILL.md"))?;
    let body = front_matter::parse(&text.text)
        .map(|front| front.body.trim().to_string())
        .unwrap_or(text.text);
    let files: Vec<Value> = listing(&skill.folder)
        .into_iter()
        .map(|file| json!({"path": file.path, "script": file.script}))
        .collect();
    let mut answer = json!({
        "name": skill.name,
        "instructions": body,
        "files": files,
    });
    if skill.has_scripts {
        answer["scripts"] = json!(
            "Run this skill's scripts with run_command. The folder is at /skill inside the sandbox."
        );
    }
    Ok(answer)
}

/// The skills the settings turn on, and the shell when one of them has
/// scripts and the sandbox works on this computer.
pub fn sources(settings: &Settings) -> Vec<Arc<dyn Source>> {
    let enabled: Vec<Skill> = discover(&roots())
        .into_iter()
        .filter(|skill| settings.skill(&skill.id).enabled)
        .collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let scripted: Vec<shell::Runnable> = enabled
        .iter()
        .filter(|skill| skill.has_scripts)
        .map(|skill| shell::Runnable {
            skill: skill.clone(),
            network: settings.skill(&skill.id).allow_network,
        })
        .collect();
    let mut sources: Vec<Arc<dyn Source>> = vec![Arc::new(Skills::new(enabled))];
    if !scripted.is_empty() {
        match sandbox::check() {
            Ok(bwrap) => sources.push(Arc::new(shell::Shell::new(bwrap, scripted))),
            Err(reason) => tracing::info!(%reason, "skill scripts cannot run"),
        }
    }
    sources
}

#[cfg(test)]
mod tests;
