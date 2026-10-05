use std::fs;
use std::path::{Path, PathBuf};

use crate::prompt::SkillEntry;

pub const AGENTS_SKILLS: &str = ".agents/skills";
pub const KYOTOAGENT_SKILLS: &str = ".kyotoagent/skills";
pub const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug)]
pub enum SkillError {
    Unknown { name: String },
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillError::Unknown { name } => write!(f, "unknown skill: {name}"),
        }
    }
}

impl std::error::Error for SkillError {}

struct Skill {
    entry: SkillEntry,
    body: String,
}

fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.as_os_str().is_empty() {
        None
    } else {
        Some(home)
    }
}

pub fn index(workspace: &Path) -> Vec<SkillEntry> {
    match home_dir() {
        Some(home) => index_in(workspace, &home),
        None => index_in(workspace, &PathBuf::new()),
    }
}

pub fn index_in(workspace: &Path, home: &Path) -> Vec<SkillEntry> {
    let mut skills: Vec<Skill> = Vec::new();
    for dir in skill_roots(workspace, home) {
        collect(&dir, &mut skills);
    }
    skills.into_iter().map(|skill| skill.entry).collect()
}

pub fn load(workspace: &Path, name: &str) -> Result<String, SkillError> {
    match home_dir() {
        Some(home) => load_in(workspace, &home, name),
        None => load_in(workspace, &PathBuf::new(), name),
    }
}

pub fn use_skill(workspace: &Path, name: &str, args: &str) -> Result<String, SkillError> {
    use_skill_allowed(workspace, name, args, None)
}

pub fn use_skill_allowed(
    workspace: &Path,
    name: &str,
    args: &str,
    allowed: Option<&[String]>,
) -> Result<String, SkillError> {
    if !skill_permitted(allowed, name) {
        return Err(SkillError::Unknown {
            name: name.to_string(),
        });
    }
    match home_dir() {
        Some(home) => use_skill_in(workspace, &home, name, args),
        None => use_skill_in(workspace, &PathBuf::new(), name, args),
    }
}

pub fn keep_allowed(skills: Vec<SkillEntry>, allowed: Option<&[String]>) -> Vec<SkillEntry> {
    let Some(allowed) = allowed else {
        return skills;
    };
    skills
        .into_iter()
        .filter(|skill| skill_permitted(Some(allowed), &skill.name))
        .collect()
}

fn skill_permitted(allowed: Option<&[String]>, name: &str) -> bool {
    match allowed {
        None => true,
        Some(names) => names.iter().any(|item| item.eq_ignore_ascii_case(name)),
    }
}

fn find_in(workspace: &Path, home: &Path, name: &str) -> Result<Skill, SkillError> {
    let mut roots = skill_roots(workspace, home);
    roots.reverse();
    for dir in roots {
        for skill_dir in skill_dirs(&dir) {
            let Some(skill) = read_skill(&skill_dir) else {
                continue;
            };
            if skill.entry.name.eq_ignore_ascii_case(name) {
                return Ok(skill);
            }
        }
    }
    Err(SkillError::Unknown {
        name: name.to_string(),
    })
}

fn load_in(workspace: &Path, home: &Path, name: &str) -> Result<String, SkillError> {
    find_in(workspace, home, name).map(|skill| skill.body)
}

fn use_skill_in(
    workspace: &Path,
    home: &Path,
    name: &str,
    args: &str,
) -> Result<String, SkillError> {
    let skill = find_in(workspace, home, name)?;
    if skill.entry.disable_model_invocation {
        let name = skill.entry.name;
        return Ok(format!("skill {name} is invoked by the user with /{name}"));
    }
    Ok(build_skill_message(&skill.entry, &skill.body, args))
}

fn break_close(text: &str, close: &str) -> String {
    let Some(head) = close.strip_suffix('>') else {
        return text.to_string();
    };
    text.replace(close, &format!("{head}\u{200b}>"))
}

fn break_skill_closes(text: &str) -> String {
    let text = break_close(text, "</skill>");
    let text = break_close(&text, "</name>");
    let text = break_close(&text, "</path>");
    break_close(&text, "</args>")
}

fn build_skill_message(entry: &SkillEntry, body: &str, args: &str) -> String {
    let mut out = String::from("<skill>\n<name>");
    out.push_str(&break_skill_closes(&entry.name));
    out.push_str("</name>\n<path>");
    out.push_str(&break_skill_closes(&entry.path));
    out.push_str("</path>\n");
    let body = break_skill_closes(body);
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    if !args.is_empty() {
        out.push_str("<args>");
        out.push_str(&break_skill_closes(args));
        out.push_str("</args>\n");
    }
    out.push_str("</skill>");
    out
}

fn skill_message(workspace: &Path, name: &str) -> Result<String, SkillError> {
    let home = home_dir().unwrap_or_default();
    let skill = find_in(workspace, &home, name)?;
    Ok(build_skill_message(&skill.entry, &skill.body, ""))
}

fn skill_roots(workspace: &Path, home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if !home.as_os_str().is_empty() {
        roots.push(home.join(KYOTOAGENT_SKILLS));
    }
    roots.push(workspace.join(KYOTOAGENT_SKILLS));
    if !home.as_os_str().is_empty() {
        roots.push(home.join(AGENTS_SKILLS));
    }
    roots.push(workspace.join(AGENTS_SKILLS));
    roots
}

fn collect(dir: &Path, skills: &mut Vec<Skill>) {
    for skill_dir in skill_dirs(dir) {
        let Some(skill) = read_skill(&skill_dir) else {
            continue;
        };
        match skills
            .iter_mut()
            .find(|existing| existing.entry.name == skill.entry.name)
        {
            Some(existing) => *existing = skill,
            None => skills.push(skill),
        }
    }
}

fn skill_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn read_skill(dir: &Path) -> Option<Skill> {
    let file = dir.join(SKILL_FILE);
    let text = fs::read_to_string(&file).ok()?;
    let dir_name = dir.file_name()?.to_string_lossy().into_owned();
    let (front, body) = split_frontmatter(&text);
    let (name, description, disable_model_invocation, user_invocable) = match &front {
        Some(front) => parse_frontmatter(front, &dir_name),
        None => (dir_name.clone(), String::new(), false, true),
    };
    Some(Skill {
        entry: SkillEntry {
            name,
            description,
            disable_model_invocation,
            user_invocable,
            path: file.to_string_lossy().into_owned(),
        },
        body,
    })
}

fn split_frontmatter(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return (None, text.to_string());
    }
    let mut front: Vec<&str> = Vec::new();
    let mut body_start = None;
    for (index, line) in lines.enumerate() {
        if line == "---" {
            body_start = Some(index + 1);
            break;
        }
        front.push(line);
    }
    let Some(start) = body_start else {
        return (None, text.to_string());
    };
    let mut body: String = text.lines().skip(start).collect::<Vec<_>>().join("\n");
    if !body.is_empty() && text.ends_with('\n') {
        body.push('\n');
    }
    (Some(front.join("\n")), body)
}

fn parse_frontmatter(front: &str, dir_name: &str) -> (String, String, bool, bool) {
    let mut name = None;
    let mut description = String::new();
    let mut disable_model_invocation = false;
    let mut user_invocable = true;
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "name" => name = Some(unquote(value)),
            "description" => description = unquote(value),
            "disable-model-invocation" => {
                if let Some(flag) = parse_bool(value) {
                    disable_model_invocation = flag;
                }
            }
            "user-invocable" => {
                if let Some(flag) = parse_bool(value) {
                    user_invocable = flag;
                }
            }
            _ => {}
        }
    }
    (
        name.unwrap_or_else(|| dir_name.to_string()),
        description,
        disable_model_invocation,
        user_invocable,
    )
}

fn parse_bool(value: &str) -> Option<bool> {
    match unquote(value).to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

pub const PICKER_LIMIT: usize = 8;

pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-'))
}

pub fn picker_token(ask: &str) -> Option<&str> {
    let rest = ask.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(&rest[..end])
}

pub fn matching<'a>(skills: &'a [SkillEntry], token: &str) -> Vec<&'a SkillEntry> {
    let needle = token.to_ascii_lowercase();
    let invocable: Vec<&SkillEntry> = skills.iter().filter(|skill| skill.user_invocable).collect();
    let mut found: Vec<&SkillEntry> = invocable
        .iter()
        .copied()
        .filter(|skill| skill.name.to_ascii_lowercase().starts_with(&needle))
        .collect();
    if found.is_empty() {
        found = invocable
            .iter()
            .copied()
            .filter(|skill| skill.name.to_ascii_lowercase().contains(&needle))
            .collect();
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.truncate(PICKER_LIMIT);
    found
}

pub fn fill_slash(name: &str) -> String {
    format!("/{name} ")
}

pub struct SlashAsk {
    pub user_text: String,
    pub skill_body: Option<String>,
    pub refused: Option<String>,
}

pub fn slash_ask(text: &str, workspace: &Path) -> SlashAsk {
    slash_ask_allowed(text, workspace, None)
}

pub fn slash_ask_allowed(text: &str, workspace: &Path, allowed: Option<&[String]>) -> SlashAsk {
    let Some(rest) = text.strip_prefix('/') else {
        return SlashAsk {
            user_text: text.to_string(),
            skill_body: None,
            refused: None,
        };
    };
    let (raw_name, remainder) = match rest.find(char::is_whitespace) {
        Some(at) => (&rest[..at], Some(rest[at..].trim_start())),
        None => (rest, None),
    };
    if !is_skill_name(raw_name) {
        return SlashAsk {
            user_text: text.to_string(),
            skill_body: None,
            refused: None,
        };
    }
    let listed = index(workspace);
    let Some(skill) = listed
        .iter()
        .find(|skill| skill.user_invocable && skill.name.eq_ignore_ascii_case(raw_name))
    else {
        return SlashAsk {
            user_text: text.to_string(),
            skill_body: None,
            refused: None,
        };
    };
    if !skill_permitted(allowed, &skill.name) {
        return SlashAsk {
            user_text: text.to_string(),
            skill_body: None,
            refused: Some(
                SkillError::Unknown {
                    name: skill.name.clone(),
                }
                .to_string(),
            ),
        };
    }
    match skill_message(workspace, &skill.name) {
        Ok(body) => {
            let user_text = match remainder {
                Some(rest) if !rest.is_empty() => rest.to_string(),
                _ => fill_slash(&skill.name).trim_end().to_string(),
            };
            SlashAsk {
                user_text,
                skill_body: Some(body),
                refused: None,
            }
        }
        Err(error) => SlashAsk {
            user_text: text.to_string(),
            skill_body: None,
            refused: Some(error.to_string()),
        },
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-skills-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the directory exists");
        dir
    }

    fn plant(root: &Path, skills_dir: &str, skill: &str, text: &str) {
        let skill_dir = root.join(skills_dir).join(skill);
        fs::create_dir_all(&skill_dir).expect("the skill directory exists");
        fs::write(skill_dir.join(SKILL_FILE), text).expect("the skill is written");
    }

    fn skill_text(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n")
    }

    fn empty_workspace(name: &str) -> (PathBuf, PathBuf) {
        let root = temp_dir(name);
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        (root, workspace)
    }

    #[test]
    fn a_global_agents_skill_is_listed_and_loads() {
        let (root, workspace) = empty_workspace("global-agents");
        let home = root.join("home");
        plant(
            &home,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "How to review a change", "Review the change."),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 1, "one skill: {skills:?}");
        assert_eq!(skills[0].name, "review");
        assert_eq!(skills[0].description, "How to review a change");

        let body = load_in(&workspace, &home, "review").expect("the skill loads");
        assert!(body.contains("Review the change."), "the body: {body}");
    }

    #[test]
    fn a_workspace_agents_skill_wins_the_same_name() {
        let (root, workspace) = empty_workspace("workspace-wins");
        let home = root.join("home");
        plant(
            &home,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "The home description.", "The home body."),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "review",
            &skill_text(
                "review",
                "The workspace description.",
                "The workspace body.",
            ),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 1, "one name, one skill: {skills:?}");
        assert_eq!(skills[0].description, "The workspace description.");

        let body = load_in(&workspace, &home, "review").expect("the skill loads");
        assert!(body.contains("The workspace body."), "the body: {body}");
        assert!(!body.contains("The home body."), "the body: {body}");
    }

    #[test]
    fn a_kyoto_home_skill_is_found() {
        let (root, workspace) = empty_workspace("kyoto-home");
        let home = root.join("home");
        plant(
            &home,
            ".kyotoagent/skills",
            "review",
            &skill_text("review", "How to review from Kyoto", "The kyoto body."),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 1, "one kyoto skill: {skills:?}");
        assert_eq!(skills[0].name, "review");
        assert_eq!(skills[0].description, "How to review from Kyoto");

        let body = load_in(&workspace, &home, "review").expect("the skill loads");
        assert!(body.contains("The kyoto body."), "the body: {body}");
    }

    #[test]
    fn a_legacy_product_skill_directory_is_not_loaded() {
        let (root, workspace) = empty_workspace("old-kyotoagent-skills");
        let home = root.join("home");
        plant(
            &home,
            ".pagent/skills",
            "review",
            &skill_text("review", "How to review a leftover", "The leftover body."),
        );
        plant(
            &workspace,
            ".pagent/skills",
            "commit",
            &skill_text("commit", "How to commit", "Commit the change."),
        );

        let skills = index_in(&workspace, &home);
        assert!(skills.is_empty(), "{skills:?}");
        assert!(load_in(&workspace, &home, "review").is_err());
        assert!(load_in(&workspace, &home, "commit").is_err());
    }

    #[test]
    fn a_missing_agents_skills_directory_is_an_empty_root() {
        let (root, workspace) = empty_workspace("missing-agents");
        let home = root.join("home");
        fs::create_dir_all(&home).expect("the home exists");
        plant(
            &workspace,
            KYOTOAGENT_SKILLS,
            "commit",
            &skill_text("commit", "How to commit", "Commit the change."),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 1, "the leftover still loads: {skills:?}");
        assert_eq!(skills[0].name, "commit");

        let body = load_in(&workspace, &home, "commit").expect("the leftover loads");
        assert!(body.contains("Commit the change."), "the body: {body}");
    }

    #[test]
    fn an_agents_skill_wins_over_a_kyotoagent_skill_with_the_same_name() {
        let (root, workspace) = empty_workspace("agents-over-kyotoagent");
        let home = root.join("home");
        plant(
            &workspace,
            KYOTOAGENT_SKILLS,
            "review",
            &skill_text("review", "The leftover description.", "The leftover body."),
        );
        plant(
            &home,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "The agents description.", "The agents body."),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 1, "one name, one skill: {skills:?}");
        assert_eq!(skills[0].description, "The agents description.");

        let body = load_in(&workspace, &home, "review").expect("the skill loads");
        assert!(body.contains("The agents body."), "the body: {body}");
        assert!(!body.contains("The leftover body."), "the body: {body}");
    }

    #[test]
    fn a_workspace_skill_is_listed_by_name_and_description() {
        let (_root, workspace) = empty_workspace("index");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "How to review", "Review the change."),
        );
        let skills = index_in(&workspace, &PathBuf::new());
        assert_eq!(skills.len(), 1, "one skill: {skills:?}");
        assert_eq!(skills[0].name, "review");
        assert_eq!(skills[0].description, "How to review");
        assert!(!skills[0].disable_model_invocation);
        assert!(skills[0].user_invocable);
    }

    #[test]
    fn a_home_skill_the_workspace_does_not_shadow_is_still_listed() {
        let (root, workspace) = empty_workspace("both");
        let home = root.join("home");
        plant(
            &home,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "How to review", "Review the change."),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "commit",
            &skill_text("commit", "How to commit.", "Commit the change."),
        );

        let skills = index_in(&workspace, &home);
        assert_eq!(skills.len(), 2, "both skills: {skills:?}");
        let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
        assert!(names.contains(&"review"), "the home skill: {names:?}");
        assert!(names.contains(&"commit"), "the workspace skill: {names:?}");
    }

    #[test]
    fn a_directory_with_no_frontmatter_name_is_addressable_by_its_directory_name() {
        let (_root, workspace) = empty_workspace("no-name");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "review",
            "---\ndescription: How to review a change\n---\n\nReview the change.\n",
        );

        let skills = index_in(&workspace, &PathBuf::new());
        assert_eq!(skills.len(), 1, "one skill: {skills:?}");
        assert_eq!(skills[0].name, "review", "the directory name");
        assert_eq!(skills[0].description, "How to review a change");

        let body = load_in(&workspace, &PathBuf::new(), "review").expect("the skill loads");
        assert!(body.contains("Review the change."), "the body: {body}");
    }

    #[test]
    fn a_file_with_no_frontmatter_at_all_is_addressable_by_its_directory_name() {
        let (_root, workspace) = empty_workspace("no-frontmatter");
        plant(&workspace, AGENTS_SKILLS, "review", "Review the change.\n");

        let skills = index_in(&workspace, &PathBuf::new());
        assert_eq!(skills.len(), 1, "one skill: {skills:?}");
        assert_eq!(skills[0].name, "review", "the directory name");

        let body = load_in(&workspace, &PathBuf::new(), "review").expect("the skill loads");
        assert!(body.contains("Review the change."), "the body: {body}");
    }

    #[test]
    fn the_body_is_everything_after_the_frontmatter() {
        let (_root, workspace) = empty_workspace("body");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "How to review", "Review the change."),
        );
        let body = load_in(&workspace, &PathBuf::new(), "review").expect("the skill loads");
        assert!(
            !body.contains("name: review"),
            "the frontmatter is not the body: {body}"
        );
        assert!(
            !body.contains("description:"),
            "the frontmatter is not the body: {body}"
        );
        assert!(body.contains("Review the change."), "the body: {body}");
    }

    #[test]
    fn an_unknown_name_is_an_error() {
        let (_root, workspace) = empty_workspace("unknown");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "review",
            &skill_text("review", "How to review", "Review the change."),
        );
        let error = load_in(&workspace, &PathBuf::new(), "nope").expect_err("no such skill");
        assert!(
            error.to_string().contains("nope"),
            "the error names the skill: {error}"
        );
    }

    #[test]
    fn a_directory_without_a_skill_file_is_not_a_skill() {
        let (_root, workspace) = empty_workspace("no-file");
        fs::create_dir_all(workspace.join(AGENTS_SKILLS).join("empty"))
            .expect("the directory exists");
        let skills = index_in(&workspace, &PathBuf::new());
        assert!(skills.is_empty(), "no skill: {skills:?}");
    }

    #[test]
    fn a_missing_skills_directory_is_an_empty_index() {
        let (_root, workspace) = empty_workspace("no-skills");
        let skills = index_in(&workspace, &PathBuf::new());
        assert!(skills.is_empty(), "no skills: {skills:?}");
    }

    #[test]
    fn a_frontmatter_that_never_closes_is_not_a_frontmatter() {
        let (front, body) = split_frontmatter("---\nname: review\n\nReview the change.\n");
        assert!(front.is_none(), "no frontmatter: {front:?}");
        assert!(
            body.contains("name: review"),
            "the whole file is the body: {body}"
        );
    }

    #[test]
    fn a_quoted_name_loses_its_quotes() {
        let (front, _body) = split_frontmatter("---\nname: \"review\"\n---\n\nBody.\n");
        let (name, _, _, _) = parse_frontmatter(&front.expect("a frontmatter"), "dir");
        assert_eq!(name, "review", "the quotes are gone: {name}");
    }

    fn entries(names: &[&str]) -> Vec<SkillEntry> {
        names
            .iter()
            .map(|name| SkillEntry {
                name: (*name).to_string(),
                description: format!("Use {name}"),
                ..SkillEntry::default()
            })
            .collect()
    }

    #[test]
    fn a_prefix_lists_every_name_that_starts_the_same_way() {
        let skills = entries(&["preview", "preflight", "commit"]);
        let names: Vec<&str> = matching(&skills, "pre")
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, vec!["preflight", "preview"]);
        let upper: Vec<&str> = matching(&skills, "PRE")
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(upper, vec!["preflight", "preview"]);
    }

    #[test]
    fn a_token_with_no_prefix_falls_back_to_names_that_contain_it() {
        let skills = entries(&["preflight", "preview", "commit"]);
        let names: Vec<&str> = matching(&skills, "view")
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, vec!["preview"]);
    }

    #[test]
    fn matching_stops_at_eight_rows() {
        let names: Vec<String> = (0..12).map(|n| format!("skill-{n:02}")).collect();
        let listed: Vec<SkillEntry> = names
            .iter()
            .map(|name| SkillEntry {
                name: name.clone(),
                description: String::new(),
                ..SkillEntry::default()
            })
            .collect();
        assert_eq!(matching(&listed, "").len(), PICKER_LIMIT);
    }

    #[test]
    fn enter_fills_the_slash_name_with_a_trailing_space() {
        assert_eq!(fill_slash("preflight"), "/preflight ");
    }

    #[test]
    fn a_known_slash_ask_loads_the_body_and_keeps_the_text_after_the_name() {
        let (_root, workspace) = empty_workspace("slash-load");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "preflight",
            &skill_text(
                "preflight",
                "Ship checks",
                "Run the closeout checks before you ship.",
            ),
        );
        let loaded = slash_ask("/preflight ship this", &workspace);
        assert_eq!(loaded.user_text, "ship this");
        let body = loaded.skill_body.expect("the body loads");
        assert!(
            body.contains("Run the closeout checks before you ship."),
            "the body: {body}"
        );
    }

    #[test]
    fn a_slash_name_with_nothing_after_it_is_the_user_message() {
        let (_root, workspace) = empty_workspace("slash-bare");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "preflight",
            &skill_text("preflight", "Ship checks", "The preflight body."),
        );
        let loaded = slash_ask("/preflight", &workspace);
        assert_eq!(loaded.user_text, "/preflight");
        assert!(loaded.skill_body.is_some());
        let spaced = slash_ask("/preflight   ", &workspace);
        assert_eq!(spaced.user_text, "/preflight");
        assert!(spaced.skill_body.is_some());
    }

    #[test]
    fn a_skill_left_out_of_the_catalog_still_loads_its_body() {
        let (root, workspace) = empty_workspace("catalog-omit");
        let home = root.join("home");
        let body = "The full instructions.";
        plant(
            &home,
            AGENTS_SKILLS,
            "longdesc",
            &skill_text("longdesc", &"d".repeat(400), body),
        );
        let skills = index_in(&workspace, &home);
        let block = crate::prompt::skills_catalog(&skills, Some(200));
        assert!(
            !block.contains("longdesc"),
            "the name is past the budget: {block}"
        );
        assert!(
            !block.contains(body),
            "the body stays out of the catalog: {block}"
        );
        assert!(
            block.contains("more skills. Call use_skill by name or the user will type /name."),
            "{block}"
        );
        let loaded = load_in(&workspace, &home, "longdesc").expect("the body loads");
        assert!(loaded.contains(body), "{loaded}");
    }

    #[test]
    fn an_unknown_slash_name_stays_an_ordinary_ask() {
        let (_root, workspace) = empty_workspace("slash-unknown");
        plant(
            &workspace,
            AGENTS_SKILLS,
            "preflight",
            &skill_text("preflight", "Ship checks", "SECRET BODY"),
        );
        let loaded = slash_ask("/unknown hi", &workspace);
        assert_eq!(loaded.user_text, "/unknown hi");
        assert!(loaded.skill_body.is_none());
    }

    #[test]
    fn picker_token_is_the_word_after_the_slash() {
        assert_eq!(picker_token("/pre"), Some("pre"));
        assert_eq!(picker_token("/preflight ship"), Some("preflight"));
        assert_eq!(picker_token("/"), Some(""));
        assert_eq!(picker_token("hello"), None);
    }

    fn flagged(name: &str, description: &str, body: &str, flags: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n{flags}---\n\n{body}\n")
    }

    fn four_skills(label: &str) -> PathBuf {
        let (_root, workspace) = empty_workspace(label);
        plant(
            &workspace,
            AGENTS_SKILLS,
            "ordinary",
            &flagged(
                "ordinary",
                "Ordinary description",
                "ORDINARY BODY",
                "license: MIT\n",
            ),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "no-model",
            &flagged(
                "no-model",
                "No model description",
                "NO MODEL BODY",
                "disable-model-invocation: true\n",
            ),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "no-user",
            &flagged(
                "no-user",
                "No user description",
                "NO USER BODY",
                "user-invocable: false\n",
            ),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "neither",
            &flagged(
                "neither",
                "Neither description",
                "NEITHER BODY",
                "disable-model-invocation: \"true\"\nuser-invocable: false\n",
            ),
        );
        plant(
            &workspace,
            AGENTS_SKILLS,
            "maybe",
            &flagged(
                "maybe",
                "Maybe description",
                "MAYBE BODY",
                "disable-model-invocation: maybe\nuser-invocable: maybe\n",
            ),
        );
        workspace
    }

    fn named<'a>(skills: &'a [SkillEntry], name: &str) -> &'a SkillEntry {
        skills
            .iter()
            .find(|skill| skill.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    #[test]
    fn the_index_records_each_invocation_flag() {
        let workspace = four_skills("flags-index");
        let skills = index_in(&workspace, &PathBuf::new());
        assert_eq!(skills.len(), 5, "five skills: {skills:?}");
        let ordinary = named(&skills, "ordinary");
        assert_eq!(ordinary.description, "Ordinary description");
        assert!(!ordinary.disable_model_invocation);
        assert!(ordinary.user_invocable);
        let no_model = named(&skills, "no-model");
        assert!(no_model.disable_model_invocation);
        assert!(no_model.user_invocable);
        let no_user = named(&skills, "no-user");
        assert!(!no_user.disable_model_invocation);
        assert!(!no_user.user_invocable);
        let neither = named(&skills, "neither");
        assert!(neither.disable_model_invocation);
        assert!(!neither.user_invocable);
        let maybe = named(&skills, "maybe");
        assert!(!maybe.disable_model_invocation);
        assert!(maybe.user_invocable);
    }

    #[test]
    fn the_prompt_lists_only_skills_the_model_may_invoke() {
        let workspace = four_skills("flags-prompt");
        let skills = index_in(&workspace, &PathBuf::new());
        let prompt =
            crate::prompt::system_prompt(&workspace.to_string_lossy(), &skills, None, "", None);
        let ordinary_path = named(&skills, "ordinary").path.clone();
        assert!(
            prompt.contains(&format!(
                "- ordinary: Ordinary description (file: {ordinary_path})"
            )),
            "{prompt}"
        );
        assert!(
            prompt.contains("- no-user: No user description"),
            "{prompt}"
        );
        assert!(prompt.contains("- maybe: Maybe description"), "{prompt}");
        assert!(!prompt.contains("no-model"), "{prompt}");
        assert!(!prompt.contains("No model description"), "{prompt}");
        assert!(!prompt.contains("neither"), "{prompt}");
        assert!(!prompt.contains("Neither description"), "{prompt}");
        assert!(!prompt.contains("ORDINARY BODY"), "{prompt}");
        assert!(!prompt.contains("NO MODEL BODY"), "{prompt}");
    }

    #[test]
    fn use_skill_returns_the_body_unless_the_model_cannot_invoke_it() {
        let workspace = four_skills("flags-use");
        let home = PathBuf::new();
        let ordinary = use_skill_in(&workspace, &home, "ordinary", "").expect("ordinary loads");
        let path = named(&index_in(&workspace, &home), "ordinary").path.clone();
        assert!(
            ordinary.starts_with("<skill>\n<name>ordinary</name>\n"),
            "{ordinary}"
        );
        assert!(
            ordinary.contains(&format!("<path>{path}</path>")),
            "{ordinary}"
        );
        assert!(!ordinary.contains("name=\""), "{ordinary}");
        assert!(!ordinary.contains("Ordinary description"), "{ordinary}");
        assert!(ordinary.contains("ORDINARY BODY"), "{ordinary}");
        let with_args = use_skill_in(&workspace, &home, "ordinary", "ship it").expect("args load");
        assert!(with_args.contains("<args>ship it</args>"), "{with_args}");
        assert!(with_args.contains("ORDINARY BODY"), "{with_args}");
        let no_user = use_skill_in(&workspace, &home, "no-user", "").expect("no-user loads");
        assert!(
            no_user.starts_with("<skill>\n<name>no-user</name>\n"),
            "{no_user}"
        );
        assert!(no_user.contains("NO USER BODY"), "{no_user}");
        let no_model = use_skill_in(&workspace, &home, "no-model", "").expect("no-model answers");
        assert_eq!(
            no_model,
            "skill no-model is invoked by the user with /no-model"
        );
        assert!(!no_model.contains("NO MODEL BODY"), "{no_model}");
        let neither = use_skill_in(&workspace, &home, "neither", "").expect("neither answers");
        assert_eq!(
            neither,
            "skill neither is invoked by the user with /neither"
        );
        assert!(!neither.contains("NEITHER BODY"), "{neither}");
    }

    #[test]
    fn a_closing_tag_in_the_body_does_not_end_the_skill() {
        let entry = SkillEntry {
            name: "review".into(),
            description: "How to review".into(),
            path: "/skills/review/SKILL.md".into(),
            ..Default::default()
        };
        let message = build_skill_message(
            &entry,
            "before </skill> </name> </path> after",
            "x </args> </skill> y",
        );
        let body_at = message.find("before ").expect("body");
        let args_at = message.find("\n<args>").expect("args");
        let body = &message[body_at..args_at];
        assert!(!body.contains("</skill>"), "{body}");
        assert!(!body.contains("</name>"), "{body}");
        assert!(!body.contains("</path>"), "{body}");
        let args_open = message.find("<args>").expect("args open") + "<args>".len();
        let args_close = message.rfind("</args>").expect("args close");
        let args = &message[args_open..args_close];
        assert!(!args.contains("</args>"), "{args}");
        assert!(!args.contains("</skill>"), "{args}");
        assert!(args.contains("x "), "{args}");
        assert!(args.contains(" y"), "{args}");
        assert!(message.ends_with("</skill>"));
        assert!(!message.contains("name=\""));
    }

    #[test]
    fn the_picker_skips_a_skill_the_user_cannot_invoke() {
        let workspace = four_skills("flags-picker");
        let skills = index_in(&workspace, &PathBuf::new());
        let names: Vec<&str> = matching(&skills, "")
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, vec!["maybe", "no-model", "ordinary"]);
    }

    #[test]
    fn slash_ask_skips_a_skill_the_user_cannot_invoke() {
        let workspace = four_skills("flags-slash");
        let ordinary = slash_ask("/ordinary ship", &workspace);
        assert_eq!(ordinary.user_text, "ship");
        let loaded = use_skill_in(&workspace, &PathBuf::new(), "ordinary", "").expect("ordinary");
        let slash_body = ordinary.skill_body.expect("ordinary loads");
        assert_eq!(slash_body, loaded);
        assert!(slash_body.contains("ORDINARY BODY"));
        assert!(slash_body.starts_with("<skill>\n<name>ordinary</name>\n"));
        let no_model = slash_ask("/no-model ship", &workspace);
        assert_eq!(no_model.user_text, "ship");
        assert!(no_model
            .skill_body
            .expect("no-model still loads for the user")
            .contains("NO MODEL BODY"));
        let no_user = slash_ask("/no-user ship", &workspace);
        assert_eq!(no_user.user_text, "/no-user ship");
        assert!(no_user.skill_body.is_none());
        let neither = slash_ask("/neither ship", &workspace);
        assert_eq!(neither.user_text, "/neither ship");
        assert!(neither.skill_body.is_none());
    }

    #[test]
    fn use_skill_outside_the_list_is_an_unknown_skill() {
        let workspace = four_skills("allow-use");
        let allowed = vec!["ordinary".to_string()];
        let loaded =
            use_skill_allowed(&workspace, "ordinary", "", Some(&allowed)).expect("ordinary loads");
        assert!(loaded.contains("ORDINARY BODY"), "{loaded}");
        let blocked =
            use_skill_allowed(&workspace, "maybe", "", Some(&allowed)).expect_err("maybe is out");
        assert_eq!(blocked.to_string(), "unknown skill: maybe");
        let empty: Vec<String> = Vec::new();
        let none = use_skill_allowed(&workspace, "ordinary", "", Some(&empty))
            .expect_err("an empty list allows nothing");
        assert_eq!(none.to_string(), "unknown skill: ordinary");
        let open = use_skill_allowed(&workspace, "maybe", "", None).expect("no list allows it");
        assert!(
            open.contains("MAYBE BODY") || open.contains("Maybe"),
            "{open}"
        );
    }

    #[test]
    fn a_slash_skill_outside_the_list_is_an_unknown_skill() {
        let workspace = four_skills("allow-slash");
        let allowed = vec!["ordinary".to_string()];
        let kept = slash_ask_allowed("/ordinary ship", &workspace, Some(&allowed));
        assert!(kept.refused.is_none());
        assert!(kept
            .skill_body
            .expect("ordinary loads")
            .contains("ORDINARY BODY"));
        let blocked = slash_ask_allowed("/maybe ship", &workspace, Some(&allowed));
        assert_eq!(blocked.refused.as_deref(), Some("unknown skill: maybe"));
        assert!(blocked.skill_body.is_none());
        assert_eq!(blocked.user_text, "/maybe ship");
        let unknown = slash_ask_allowed("/not-a-skill ship", &workspace, Some(&allowed));
        assert!(unknown.refused.is_none());
        assert!(unknown.skill_body.is_none());
        assert_eq!(unknown.user_text, "/not-a-skill ship");
        let listed = keep_allowed(index_in(&workspace, &PathBuf::new()), Some(&allowed));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "ordinary");
        assert!(keep_allowed(index_in(&workspace, &PathBuf::new()), Some(&[])).is_empty());
    }
}
