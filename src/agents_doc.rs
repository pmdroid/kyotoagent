use std::fs;
use std::path::{Path, PathBuf};

const CAP: usize = 32_768;
const TAIL: usize = 8 * 1024;
const TRUNCATED: &str = "AGENTS.md truncated.";

pub fn load(workspace: &Path) -> String {
    match home_dir() {
        Some(home) => load_in(workspace, &home),
        None => load_in(workspace, Path::new("")),
    }
}

pub fn load_in(workspace: &Path, home: &Path) -> String {
    let mut parts = Vec::new();
    if !home.as_os_str().is_empty() {
        for folder in [".kyotoagent", ".agents"] {
            if let Some(text) = read_regular(&home.join(folder).join("AGENTS.md")) {
                parts.push(text);
            }
        }
    }
    let root = project_root(workspace);
    for dir in directories(&root, workspace) {
        if let Some(text) = file_in_dir(&root, &dir) {
            parts.push(text);
        }
    }
    join_capped(&parts)
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

fn project_root(workspace: &Path) -> PathBuf {
    let mut current = workspace.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return current;
        }
        if !current.pop() {
            return workspace.to_path_buf();
        }
    }
}

fn directories(root: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = workspace.to_path_buf();
    loop {
        dirs.push(current.clone());
        if current == root {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    dirs.reverse();
    dirs
}

fn file_in_dir(root: &Path, dir: &Path) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = read_inside(root, &dir.join("AGENTS.md")) {
        parts.push(text);
    }
    for folder in [".kyotoagent", ".agents"] {
        if let Some(text) = named_in(root, &dir.join(folder)) {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn named_in(root: &Path, dir: &Path) -> Option<String> {
    let upper = dir.join("AGENTS.md");
    if exists(&upper) {
        return read_inside(root, &upper);
    }
    let lower = dir.join("agents.md");
    if exists(&lower) {
        read_inside(root, &lower)
    } else {
        None
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn read_regular(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn read_inside(root: &Path, path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let resolved = fs::canonicalize(path).ok()?;
    let root_real = fs::canonicalize(root).ok()?;
    if !resolved.starts_with(&root_real) {
        return None;
    }
    read_regular(&resolved)
}

fn join_capped(parts: &[String]) -> String {
    let mut combined = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            combined.push_str("\n\n");
        }
        combined.push_str(part);
    }
    if combined.len() <= CAP {
        return combined;
    }
    let head_end = combined.floor_char_boundary(CAP);
    let tail_start = combined.floor_char_boundary(combined.len().saturating_sub(TAIL));
    let mut out = combined[..head_end].to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(TRUNCATED);
    out.push('\n');
    out.push_str(&combined[tail_start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kyotoagent-agents-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the directory exists");
        dir
    }

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Scratch {
            Scratch {
                root: temp_dir(name),
            }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the parent exists");
        }
        fs::write(path, text).expect("the file writes");
    }

    fn git_root(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).expect("the git dir exists");
    }

    #[test]
    fn a_workspace_with_no_file_adds_nothing() {
        let scratch = Scratch::new("missing");
        let workspace = scratch.path("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let home = scratch.path("home");
        fs::create_dir_all(&home).expect("the home exists");
        assert!(load_in(&workspace, &home).is_empty());
    }

    #[test]
    fn the_git_root_file_is_in_the_text() {
        let scratch = Scratch::new("root");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "pnpm test\n");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("pnpm test"), "{text}");
        assert!(!text.contains("truncated"), "{text}");
    }

    #[test]
    fn a_file_above_the_git_root_stays_out() {
        let scratch = Scratch::new("above");
        write(&scratch.path("AGENTS.md"), "from the parent\n");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("from the parent"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn the_closer_file_comes_after_the_root() {
        let scratch = Scratch::new("nested");
        let repo = scratch.path("repo");
        git_root(&repo);
        write(&repo.join("AGENTS.md"), "root rule\n");
        let pkg = repo.join("pkg");
        write(&pkg.join("AGENTS.md"), "pkg rule\n");
        let text = load_in(&pkg, &scratch.path("home"));
        let root_at = text.find("root rule").expect("the root file");
        let pkg_at = text.find("pkg rule").expect("the pkg file");
        assert!(root_at < pkg_at, "{text}");
        assert!(text.contains("\n\n"), "{text}");
    }

    #[test]
    fn an_intermediate_directory_keeps_its_place() {
        let scratch = Scratch::new("mid");
        let repo = scratch.path("repo");
        git_root(&repo);
        write(&repo.join("AGENTS.md"), "ROOT\n");
        write(&repo.join("mid").join("AGENTS.md"), "MID\n");
        let pkg = repo.join("mid").join("pkg");
        write(&pkg.join("AGENTS.md"), "PKG\n");
        let text = load_in(&pkg, &scratch.path("home"));
        let root_at = text.find("ROOT").expect("root");
        let mid_at = text.find("MID").expect("mid");
        let pkg_at = text.find("PKG").expect("pkg");
        assert!(root_at < mid_at && mid_at < pkg_at, "{text}");
    }

    #[test]
    fn a_directory_whose_only_file_is_claude_md_adds_nothing() {
        let scratch = Scratch::new("claude");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("CLAUDE.md"), "use cargo test\n");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn an_empty_agents_md_adds_nothing() {
        let scratch = Scratch::new("empty");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "\n");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn the_kyoto_home_file_comes_before_the_agents_home_file() {
        let scratch = Scratch::new("global");
        let home = scratch.path("home");
        let line = "🐕 Written by Kyoto, an AI agent, on Pascal's behalf —";
        write(
            &home.join(".kyotoagent").join("AGENTS.md"),
            &format!("KYOTO\n{line}\n"),
        );
        write(
            &home.join(".agents").join("AGENTS.md"),
            &format!("DOTHOME\n{line}\n"),
        );
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "ROOT\n");
        let text = load_in(&workspace, &home);
        let kyoto_at = text.find("KYOTO").expect("the kyoto home file");
        let home_at = text.find("DOTHOME").expect("the agents home file");
        let root_at = text.find("ROOT").expect("the root file");
        assert!(kyoto_at < home_at && home_at < root_at, "{text}");
        assert!(text.contains(line), "{text}");
        let system = crate::prompt::system_prompt("/w", &[], None, &text, None);
        let prompt_kyoto = system.find("KYOTO").expect("kyoto in the prompt");
        let prompt_home = system.find("DOTHOME").expect("agents home in the prompt");
        assert!(prompt_kyoto < prompt_home, "{system}");
        assert!(system.contains(line), "{system}");
    }

    #[test]
    fn a_kyotoagent_home_file_adds_nothing() {
        let scratch = Scratch::new("kyotoagent-home");
        let home = scratch.path("home");
        write(&home.join(".pagent").join("AGENTS.md"), "FROM KYOTOAGENT\n");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let text = load_in(&workspace, &home);
        assert!(!text.contains("FROM KYOTOAGENT"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_kyotoagent_repo_file_adds_nothing() {
        let scratch = Scratch::new("kyotoagent-repo");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(
            &workspace.join(".pagent").join("AGENTS.md"),
            "FROM KYOTOAGENT REPO\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("FROM KYOTOAGENT REPO"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_file_past_the_cap_is_cut_on_a_char_boundary() {
        let scratch = Scratch::new("cap");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let mut body = "x".repeat(32_767);
        body.push('é');
        body.push_str(&"y".repeat(8_000));
        write(&workspace.join("AGENTS.md"), &body);
        let text = load_in(&workspace, &scratch.path("home"));
        let notice = text
            .find("AGENTS.md truncated.")
            .expect("the truncation line");
        let head = &text[..notice];
        assert!(head.ends_with('\n'), "{head:?}");
        let kept = head.trim_end_matches('\n');
        assert_eq!(kept.len(), 32_767, "the cut stops before the split char");
        assert!(kept.chars().all(|ch| ch == 'x'), "no partial char");
        let tail = &body[body.len() - (8 * 1024)..];
        assert_eq!(tail.len(), 8 * 1024);
        assert!(text.ends_with(tail), "{text}");
        assert!(tail.contains('é'), "{tail}");
        assert!(tail.contains(&"y".repeat(8_000)), "{tail}");
    }

    #[test]
    fn a_forty_kib_file_stops_at_the_cap() {
        let scratch = Scratch::new("forty");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), &"x".repeat(40 * 1024));
        let text = load_in(&workspace, &scratch.path("home"));
        let notice = text.find("AGENTS.md truncated.").expect("the line");
        let kept = text[..notice].trim_end_matches('\n');
        assert_eq!(kept.len(), CAP);
        assert!(kept.chars().all(|ch| ch == 'x'));
    }

    #[test]
    fn a_second_file_is_cut_once_the_cap_is_full() {
        let scratch = Scratch::new("two");
        let repo = scratch.path("repo");
        git_root(&repo);
        write(&repo.join("AGENTS.md"), &"a".repeat(20_000));
        let pkg = repo.join("pkg");
        write(
            &pkg.join("AGENTS.md"),
            &format!("TAIL{}", "b".repeat(20_000)),
        );
        let text = load_in(&pkg, &scratch.path("home"));
        assert!(text.contains(&"a".repeat(20_000)), "the root file is whole");
        assert!(text.contains("TAIL"), "the closer file starts");
        assert!(text.contains("AGENTS.md truncated."));
        let notice = text.find("AGENTS.md truncated.").expect("the line");
        assert!(text[..notice].trim_end_matches('\n').len() <= CAP);
    }

    #[test]
    fn a_symlink_inside_the_project_is_read() {
        let scratch = Scratch::new("link-in");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("notes.md"), "pnpm test\n");
        std::os::unix::fs::symlink(workspace.join("notes.md"), workspace.join("AGENTS.md"))
            .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("pnpm test"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_symlink_that_leaves_the_project_is_skipped() {
        let scratch = Scratch::new("link-out");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&scratch.path("outside.md"), "secret outside\n");
        std::os::unix::fs::symlink(scratch.path("outside.md"), workspace.join("AGENTS.md"))
            .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("secret outside"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_skill_body_stays_out_of_the_agents_text() {
        let scratch = Scratch::new("skill");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "pnpm test\n");
        let skill = workspace.join(".agents").join("skills").join("review");
        write(
            &skill.join("SKILL.md"),
            "---\nname: review\ndescription: How to review\n---\n\nSECRET SKILL BODY\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("pnpm test"), "{text}");
        assert!(!text.contains("SECRET SKILL BODY"), "{text}");
    }

    #[test]
    fn a_dot_agents_file_is_in_the_text() {
        let scratch = Scratch::new("dot");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(
            &workspace.join(".agents").join("AGENTS.md"),
            "dot agents rule\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("dot agents rule"), "{text}");
    }

    #[test]
    fn a_lower_case_dot_agents_file_fills_in_when_the_upper_one_is_absent() {
        let scratch = Scratch::new("dot-lower");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join(".agents").join("agents.md"), "from lower\n");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("from lower"), "{text}");
    }

    #[test]
    fn the_root_file_comes_before_the_dot_agents_file() {
        let scratch = Scratch::new("both-agents");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "ROOT\n");
        write(&workspace.join(".agents").join("AGENTS.md"), "NESTED\n");
        let text = load_in(&workspace, &scratch.path("home"));
        let root_at = text.find("ROOT").expect("the root file");
        let nested_at = text.find("NESTED").expect("the dot agents file");
        assert!(root_at < nested_at, "{text}");
        assert!(text.contains("\n\n"), "{text}");
    }

    #[test]
    fn an_empty_dot_agents_file_does_not_fall_through() {
        let scratch = Scratch::new("dot-empty");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join(".agents").join("agents.md"), "from lower\n");
        write(&workspace.join(".agents").join("AGENTS.md"), "\n");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_lower_case_kyotoagent_file_fills_in_when_the_upper_one_is_absent() {
        let scratch = Scratch::new("kyoto-lower");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(
            &workspace.join(".kyotoagent").join("agents.md"),
            "from kyoto lower\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("from kyoto lower"), "{text}");
    }

    #[test]
    fn the_kyotoagent_file_comes_before_the_dot_agents_file() {
        let scratch = Scratch::new("both-folders");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("AGENTS.md"), "ROOT\n");
        write(&workspace.join(".kyotoagent").join("AGENTS.md"), "KYOTO\n");
        write(&workspace.join(".agents").join("AGENTS.md"), "NESTED\n");
        let text = load_in(&workspace, &scratch.path("home"));
        let root_at = text.find("ROOT").expect("the root file");
        let kyoto_at = text.find("KYOTO").expect("the kyoto file");
        let nested_at = text.find("NESTED").expect("the dot agents file");
        assert!(root_at < kyoto_at && kyoto_at < nested_at, "{text}");
    }

    #[test]
    fn a_skill_file_alone_adds_nothing() {
        let scratch = Scratch::new("skill-only");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let skill = workspace.join(".agents").join("skills").join("review");
        write(
            &skill.join("SKILL.md"),
            "---\nname: review\ndescription: How to review\n---\n\nSECRET SKILL BODY\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.is_empty(), "{text}");
        assert!(!text.contains("SECRET SKILL BODY"), "{text}");
    }

    #[test]
    fn a_dot_agents_symlink_that_leaves_the_project_is_skipped() {
        let scratch = Scratch::new("dot-link-out");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&scratch.path("outside.md"), "secret outside\n");
        fs::create_dir_all(workspace.join(".agents")).expect("the agents dir exists");
        std::os::unix::fs::symlink(
            scratch.path("outside.md"),
            workspace.join(".agents").join("AGENTS.md"),
        )
        .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("secret outside"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_kyotoagent_symlink_inside_the_project_is_skipped() {
        let scratch = Scratch::new("kyoto-link-in");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&workspace.join("notes.md"), "pnpm test\n");
        fs::create_dir_all(workspace.join(".kyotoagent")).expect("the kyoto dir exists");
        std::os::unix::fs::symlink(
            workspace.join("notes.md"),
            workspace.join(".kyotoagent").join("AGENTS.md"),
        )
        .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("pnpm test"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_kyotoagent_symlink_that_leaves_the_project_is_skipped() {
        let scratch = Scratch::new("kyoto-link-out");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&scratch.path("outside.md"), "secret outside\n");
        fs::create_dir_all(workspace.join(".kyotoagent")).expect("the kyoto dir exists");
        std::os::unix::fs::symlink(
            scratch.path("outside.md"),
            workspace.join(".kyotoagent").join("AGENTS.md"),
        )
        .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(!text.contains("secret outside"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_home_symlink_adds_nothing() {
        let scratch = Scratch::new("home-link");
        let home = scratch.path("home");
        write(&scratch.path("outside.md"), "secret outside\n");
        for folder in [".kyotoagent", ".agents"] {
            fs::create_dir_all(home.join(folder)).expect("the home folder exists");
            std::os::unix::fs::symlink(
                scratch.path("outside.md"),
                home.join(folder).join("AGENTS.md"),
            )
            .expect("the link is made");
        }
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let text = load_in(&workspace, &home);
        assert!(!text.contains("secret outside"), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn a_regular_dot_agents_file_loads_beside_a_skipped_symlink() {
        let scratch = Scratch::new("beside");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&scratch.path("outside.md"), "secret outside\n");
        std::os::unix::fs::symlink(scratch.path("outside.md"), workspace.join("AGENTS.md"))
            .expect("the link is made");
        write(
            &workspace.join(".agents").join("AGENTS.md"),
            "dot agents rule\n",
        );
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("dot agents rule"), "{text}");
        assert!(!text.contains("secret outside"), "{text}");
    }

    #[test]
    fn a_regular_kyotoagent_file_loads_when_dot_agents_is_a_symlink() {
        let scratch = Scratch::new("kyoto-beside");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(&scratch.path("outside.md"), "secret outside\n");
        write(
            &workspace.join(".kyotoagent").join("AGENTS.md"),
            "kyoto rule\n",
        );
        fs::create_dir_all(workspace.join(".agents")).expect("the agents dir exists");
        std::os::unix::fs::symlink(
            scratch.path("outside.md"),
            workspace.join(".agents").join("AGENTS.md"),
        )
        .expect("the link is made");
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("kyoto rule"), "{text}");
        assert!(!text.contains("secret outside"), "{text}");
    }

    #[test]
    fn a_file_past_the_cap_keeps_the_attribution_line_in_the_system_prompt() {
        let scratch = Scratch::new("attribution-tail");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        let line = "🐕 Written by Kyoto, an AI agent, on Pascal's behalf —";
        let mut body = "x".repeat(40_000);
        body.push('\n');
        body.push_str(line);
        body.push('\n');
        write(&workspace.join("AGENTS.md"), &body);
        let text = load_in(&workspace, &scratch.path("home"));
        assert!(text.contains("AGENTS.md truncated."), "{text}");
        assert!(text.contains(line), "{text}");
        let system = crate::prompt::system_prompt("/w", &[], None, &text, None);
        assert!(system.contains(line), "{system}");
        assert!(
            system.contains("You are Kyoto Agent, a coding agent working in /w."),
            "{system}"
        );
    }

    #[test]
    fn a_dot_agents_body_is_in_the_system_prompt_and_the_subagent_prompt() {
        let scratch = Scratch::new("prompt");
        let workspace = scratch.path("repo");
        git_root(&workspace);
        write(
            &workspace.join(".agents").join("AGENTS.md"),
            "dot agents rule\n",
        );
        let agents = load_in(&workspace, &scratch.path("home"));
        let system = crate::prompt::system_prompt("/w", &[], None, &agents, None);
        let child = crate::prompt::subagent_prompt("/w", &[], None, &agents, None);
        assert!(
            system.starts_with("You are Kyoto Agent, a coding agent working in /w."),
            "{system}"
        );
        assert!(
            child.starts_with("You are a Kyoto Agent subagent."),
            "{child}"
        );
        for prompt in [&system, &child] {
            let heading = prompt
                .find("Project instructions (AGENTS.md):")
                .expect("the section");
            let body = prompt.find("dot agents rule").expect("the body");
            assert!(heading < body, "{prompt}");
        }
    }
}
