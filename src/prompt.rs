//! The system prompt the model sees at the start of a turn.
//!
//! The prompt is short on purpose: the workspace path, the tool rules, the
//! skill index (name and description only), and a line telling the model to
//! call `ask` when a decision is missing and `finish` with the result. Skill
//! bodies stay out of this prompt. The model loads a body with
//! `use_skill` when it needs it, so a body here would be a cost every turn
//! pays whether it uses the skill or not.

use crate::closeout::CloseoutFile;

/// One skill the model may use, as the prompt lists it: a name and a
/// description. The body is deliberately not here.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub disable_model_invocation: bool,
    #[serde(default = "default_user_invocable")]
    pub user_invocable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

fn default_user_invocable() -> bool {
    true
}

impl Default for SkillEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            disable_model_invocation: false,
            user_invocable: true,
            path: String::new(),
        }
    }
}

const UNKNOWN_CATALOG_CHARS: usize = 8_000;
const WORK_INSTRUCTIONS: &str = include_str!("prompts/work.md");

fn catalog_shell(body: &str) -> String {
    let mut out = String::from("<skills_instructions>\n## Skills\n");
    out.push_str("Before work, you must use_skill for skills the user names or whose descriptions clearly match the task.\n");
    out.push_str("### Available skills\n");
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</skills_instructions>\n");
    out
}

pub struct PromptParts {
    pub without_skills: String,
    pub skills: String,
    pub full: String,
}

pub fn catalog_char_budget(context_length: Option<u64>) -> usize {
    match context_length {
        Some(tokens) if tokens > 0 => {
            let share = (tokens.saturating_mul(2) / 100).max(1);
            usize::try_from(share.saturating_mul(4)).unwrap_or(usize::MAX)
        }
        _ => UNKNOWN_CATALOG_CHARS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogFit {
    Fits,
    DescriptionsShortened { kept: usize },
    NamesOnly { listed: usize },
    NamesDropped { listed: usize, omitted: usize },
}

pub fn catalog_fit(skills: &[SkillEntry], context_length: Option<u64>) -> CatalogFit {
    let rendered = skills_catalog(skills, context_length);
    let visible: Vec<&SkillEntry> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if visible.is_empty() || rendered == render_block(&visible_owned(&visible), None) {
        return CatalogFit::Fits;
    }
    let listed = rendered
        .lines()
        .filter(|line| line.starts_with("- "))
        .count();
    if rendered.contains(" more skills.") {
        return CatalogFit::NamesDropped {
            listed,
            omitted: visible.len().saturating_sub(listed),
        };
    }
    if rendered.lines().any(|line| line.contains(": ")) {
        return CatalogFit::DescriptionsShortened {
            kept: description_limit(&rendered),
        };
    }
    CatalogFit::NamesOnly { listed }
}

fn visible_owned(skills: &[&SkillEntry]) -> Vec<SkillEntry> {
    skills.iter().map(|skill| (*skill).clone()).collect()
}

fn description_limit(rendered: &str) -> usize {
    rendered
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .filter_map(|line| line.split_once(": "))
        .map(|(_, rest)| {
            rest.strip_suffix(')')
                .and_then(|rest| rest.rsplit_once(" (file: "))
                .map(|(description, _)| description)
                .unwrap_or(rest)
        })
        .map(|description| description.chars().count())
        .max()
        .unwrap_or(0)
}

pub fn skills_catalog(skills: &[SkillEntry], context_length: Option<u64>) -> String {
    let skills: Vec<SkillEntry> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .cloned()
        .collect();
    if skills.is_empty() {
        return String::new();
    }
    let skills = skills.as_slice();
    let budget = catalog_char_budget(context_length);
    let full = render_block(skills, None);
    if fits(&full, budget) {
        return full;
    }
    if let Some(shortened) = longest_descriptions(skills, budget) {
        return shortened;
    }
    let names = render_block(skills, Some(0));
    if fits(&names, budget) {
        return names;
    }
    fit_names(skills, budget)
}

fn fits(text: &str, budget: usize) -> bool {
    text.chars().count() <= budget
}

fn skill_line(skill: &SkillEntry, limit: Option<usize>) -> String {
    let description = match limit {
        None => skill.description.clone(),
        Some(0) => String::new(),
        Some(limit) => skill.description.chars().take(limit).collect(),
    };
    if description.is_empty() {
        if skill.path.is_empty() {
            format!("- {}\n", skill.name)
        } else {
            format!("- {}: (file: {})\n", skill.name, skill.path)
        }
    } else if skill.path.is_empty() {
        format!("- {}: {}\n", skill.name, description)
    } else {
        format!("- {}: {} (file: {})\n", skill.name, description, skill.path)
    }
}

fn render_block(skills: &[SkillEntry], limit: Option<usize>) -> String {
    let mut body = String::new();
    for skill in skills {
        body.push_str(&skill_line(skill, limit));
    }
    catalog_shell(&body)
}

fn longest_descriptions(skills: &[SkillEntry], budget: usize) -> Option<String> {
    let max_desc = skills
        .iter()
        .map(|skill| skill.description.chars().count())
        .max()
        .unwrap_or(0);
    let mut best = None;
    let mut lo = 0usize;
    let mut hi = max_desc;
    loop {
        let mid = lo + (hi - lo) / 2;
        let rendered = render_block(skills, Some(mid));
        if fits(&rendered, budget) {
            best = Some(rendered);
            if mid == hi {
                break;
            }
            lo = mid + 1;
            if lo > hi {
                break;
            }
        } else if mid == lo {
            break;
        } else {
            hi = mid - 1;
            if lo > hi {
                break;
            }
        }
    }
    best
}

fn more_line(omitted: usize) -> String {
    format!("{omitted} more skills. Call use_skill by name or the user will type /name.\n")
}

const FINISH_LINE: &str = "\
The user sees questions, permissions, and the result. Text you emit while calling tools never reaches the screen. A proof card is a second card, so leave `proof` empty unless it adds evidence the result does not already state.

When you need a decision you do not have, call `ask`. Ask when a choice is destructive, or when the request is ambiguous in a way that changes the outcome. Finish the unblocked work first, then ask one question.

Keep going until the request is done. Implement when asked. Answer when asked.

When the work is done, call `finish` with `text`. A reply with no tools becomes a result and no proof card.
`text` is the result card. It stands alone for a reader who has not seen your tools. Lead with the outcome. Name files as `path` or `path:line`. Quote a snippet only when the user asked to see it.
`proof` is optional context for the parent agent and never publishes a file artifact. Use it only for evidence the parent needs that `text` does not already state. Do not restate `text`. Claim a fix, a test, or a done task only when a tool result supports it.
A task exit is not a result, and finish waits until the tasks started on this turn have exited.

Add no comments. Commit only when the user asks. Use URLs from the user or from files you read. Leave other people's uncommitted work alone. Run `git reset --hard` or `git checkout --` only when the user asks.
";

pub const ARTIFACT_LINE: &str = "You decide which useful files to share. `attach_artifact` is the only tool that publishes clickable chat artifacts and retains session copies. Attach requested deliverables or evidence supporting a meaningful claim: Markdown reports, screenshots/images, videos or relevant terminal transcripts. Do not attach routine tool output, transient lookup errors or a duplicate response merely to finish a turn. Answers, clarification and progress need no attachment. Include git_sha when documenting or verifying a specific commit. Exercise the feature and capture its observed result; never invent verification data. Correct attachment errors and give a short matching response. Code changes need appropriate verification, which can be a recorded check without an artifact. Closeout retains actual check transcripts without publishing them; use the returned file_id with attach_artifact only when useful to share. An attachment never passes a check. Keep failure, timeout and stale outcomes truthful.\n";

const DELEGATE_LINE: &str = "When the user asks you to delegate, launching the children is part of doing the work, so call `spawn_subagent` near the start. Independent children belong in one turn. Leave `run_in_background` true. A child is hidden from the session list. Set `visible` true only when the user should watch that child. Call `check_task` with every id when you need all of them. timeout_sec waits until every listed child is idle.\nWhen you are done with a child, call `kill_task` with its id. That removes the session and its worktree. Clean up every child you started before you finish the turn.\n`archive_session` only archives. It stops the turn and hides the session. The directory and the log stay. It cannot restore or delete a session.\n";

fn agents_block(agents: &str) -> String {
    if agents.is_empty() {
        return String::new();
    }
    let mut block = String::from("Project instructions (AGENTS.md):\n\n");
    block.push_str(agents);
    if !agents.ends_with('\n') {
        block.push('\n');
    }
    block.push('\n');
    block
}

fn closeout_block(closeout: Option<&CloseoutFile>, child: bool) -> String {
    let Some(_) = closeout else {
        return String::new();
    };
    let mut suffix = String::from(
        "This workspace has closeout checks. After changes and before finishing, call `get_closeout` to discover the required checks and their current status. Run each pending ID in order with `run_closeout`, then call `get_closeout` again. The run_closeout tool runs matching setup steps before checks. Pass a different model to run_closeout when a review has different_model set. Further workspace edits can make passed checks stale. The turn cannot finish until every required check has passed. If blocked is non-null, stop retrying and report the blocker.\n",
    );
    if child {
        suffix.push_str(
            "If a required check fails, fix it and call run_closeout again. If you cannot fix a required check, call finish with the error and what you tried.\n\n",
        );
    } else {
        suffix.push_str(
            "If a required check fails, fix it and call run_closeout again. If you cannot fix it, call ask with the error and what you tried.\n\n",
        );
    }
    suffix
}

fn fit_names(skills: &[SkillEntry], budget: usize) -> String {
    let mut chosen = None;
    for keep in 0..=skills.len() {
        let omitted = skills.len() - keep;
        let mut body = String::new();
        for skill in skills.iter().take(keep) {
            body.push_str(&skill_line(skill, Some(0)));
        }
        if omitted > 0 {
            body.push_str(&more_line(omitted));
        }
        let out = catalog_shell(&body);
        if fits(&out, budget) {
            chosen = Some(out);
        }
    }
    chosen.unwrap_or_else(|| catalog_shell(&more_line(skills.len())))
}

pub fn prompt_parts(
    workspace: &str,
    skills: &[SkillEntry],
    closeout: Option<&CloseoutFile>,
    agents: &str,
    context_length: Option<u64>,
) -> PromptParts {
    let mut prefix = String::new();
    prefix.push_str("You are Kyoto Agent, a coding agent working in ");
    prefix.push_str(workspace);
    prefix.push_str(".\n\n");
    prefix.push_str(WORK_INSTRUCTIONS);
    prefix.push('\n');
    prefix.push_str(&agents_block(agents));

    let skills_block = skills_catalog(skills, context_length);
    let mut suffix = closeout_block(closeout, false);
    suffix.push_str(FINISH_LINE);
    suffix.push_str(ARTIFACT_LINE);
    suffix.push_str(DELEGATE_LINE);
    suffix.push_str(
        "Keep the todo list current. Replace it as the work moves. Put the one-line step in title. Put the plan in description. Attach workspace paths and doc URLs you will need later. Mark the step you are on in_progress. Mark a finished step done.\n",
    );
    suffix.push_str(
        "Put helper scripts and commit-message drafts under /tmp/ and delete them when you are done.\n",
    );
    let mut full = prefix.clone();
    full.push_str(&skills_block);
    full.push_str(&suffix);
    let mut without_skills = prefix;
    without_skills.push_str(&suffix);
    PromptParts {
        without_skills,
        skills: skills_block,
        full,
    }
}

pub fn system_prompt(
    workspace: &str,
    skills: &[SkillEntry],
    closeout: Option<&CloseoutFile>,
    agents: &str,
    context_length: Option<u64>,
) -> String {
    prompt_parts(workspace, skills, closeout, agents, context_length).full
}

pub fn subagent_prompt(
    workspace: &str,
    skills: &[SkillEntry],
    closeout: Option<&CloseoutFile>,
    agents: &str,
    context_length: Option<u64>,
) -> String {
    let mut text = String::from("You are a Kyoto Agent subagent. The workspace is ");
    text.push_str(workspace);
    text.push_str(".\n\n");
    text.push_str(
        "You work for the parent agent. Call `finish` with `text` when the task is done. Leave `proof` empty unless it adds evidence `text` does not already state. That is how the parent receives the work.\n\n",
    );
    text.push_str(
        "If a decision is missing, call `finish` anyway. Put what you did in `text`, and name the decision you needed. Leave `proof` empty when it would only repeat `text`.\n\n",
    );
    text.push_str(WORK_INSTRUCTIONS);
    text.push('\n');
    text.push_str(&agents_block(agents));
    text.push_str(&skills_catalog(skills, context_length));
    text.push_str(&closeout_block(closeout, true));
    text.push_str(ARTIFACT_LINE);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_the_workspace_and_the_two_special_tools() {
        let prompt = system_prompt("/w", &[], None, "", None);
        assert!(prompt.contains("/w"), "the workspace: {prompt}");
        assert!(!prompt.contains("at most"), "no call cap: {prompt}");
        assert!(
            !prompt.contains("model call"),
            "no model-call count: {prompt}"
        );
        assert!(prompt.contains("`ask`"), "the ask instruction: {prompt}");
        assert!(
            prompt.contains("`finish`"),
            "the finish instruction: {prompt}"
        );
        assert!(
            prompt.contains(
                "leave `proof` empty unless it adds evidence the result does not already state"
            ),
            "the finish instruction leaves a repeated proof empty: {prompt}"
        );
        assert!(prompt.contains("todo"), "the todo tool: {prompt}");
        assert!(
            prompt.contains("Keep the todo list current"),
            "the todo instruction: {prompt}"
        );
        assert!(prompt.contains("title"), "the todo title: {prompt}");
        assert!(
            prompt.contains("description"),
            "the todo description: {prompt}"
        );
        assert!(
            prompt.contains("/tmp/"),
            "scratch files stay under /tmp: {prompt}"
        );
        assert!(
            !prompt.contains("You have these tools"),
            "tool one-liners stay off the prompt: {prompt}"
        );
        let tools = serde_json::to_string(&crate::turn::tool_definitions(
            &crate::config::Config::default(),
            false,
        ))
        .expect("tools");
        for name in [
            "read_file",
            "list_dir",
            "write_file",
            "use_skill",
            "ask",
            "finish",
        ] {
            assert!(tools.contains(name), "the tool definition {name}");
        }
    }

    #[test]
    fn an_empty_skill_index_still_builds_the_prompt() {
        let prompt = system_prompt("/w", &[], None, "", None);
        let child = subagent_prompt("/w", &[], None, "", None);
        assert!(
            prompt.starts_with("You are Kyoto Agent, a coding agent working in /w."),
            "{prompt}"
        );
        assert!(
            child.starts_with("You are a Kyoto Agent subagent."),
            "{child}"
        );
        for built in [&prompt, &child] {
            assert!(!built.contains("You are kyotoagent"), "{built}");
            assert!(!built.contains("You are a kyotoagent subagent"), "{built}");
            assert!(
                !built.contains(
                    "When the work is done, call `finish` with the result and the proof."
                ),
                "{built}"
            );
        }
        assert!(prompt.contains("never reaches the screen"), "{prompt}");
        assert!(prompt.contains("call `finish` with `text`."), "{prompt}");
        assert!(prompt.contains("`text` is the result card."), "{prompt}");
        assert!(
            prompt.contains(
                "leave `proof` empty unless it adds evidence the result does not already state"
            ),
            "{prompt}"
        );
        assert!(prompt.contains("call `ask`"), "{prompt}");
        assert!(prompt.contains("Claim a fix"), "{prompt}");
        assert!(
            prompt.contains(
                "A task exit is not a result, and finish waits until the tasks started on this turn have exited."
            ),
            "{prompt}"
        );
        assert!(prompt.contains("Add no comments"), "{prompt}");
        assert!(prompt.contains("git reset --hard"), "{prompt}");
        assert!(child.contains("You work for the parent agent"), "{child}");
        assert!(
            child.contains(
                "Call `finish` with `text` when the task is done. Leave `proof` empty unless it adds evidence `text` does not already state."
            ),
            "{child}"
        );
        assert!(!child.contains("call `ask`"), "{child}");
        assert!(
            child.contains("If a decision is missing, call `finish` anyway."),
            "{child}"
        );
        assert!(
            child.contains("Leave `proof` empty when it would only repeat `text`."),
            "{child}"
        );
        assert!(prompt.contains("spawn_subagent"), "{prompt}");
        assert!(prompt.contains("Keep the todo list current"), "{prompt}");
        assert!(prompt.contains("/tmp/"), "{prompt}");
        assert!(!child.contains("spawn_subagent"), "{child}");
        assert!(!child.contains("Keep the todo list current"), "{child}");
        assert!(
            !prompt.contains("<skills_instructions>"),
            "no skill section when there are none"
        );
        assert!(
            FINISH_LINE.chars().count() < 1600,
            "{}",
            FINISH_LINE.chars().count()
        );
        assert!(
            !FINISH_LINE.contains("🐕 Written by Kyoto, an AI agent, on Pascal's behalf —"),
            "{FINISH_LINE}"
        );
        assert!(
            !prompt.contains("🐕 Written by Kyoto, an AI agent, on Pascal's behalf —"),
            "{prompt}"
        );
        let tools = serde_json::to_string(&crate::turn::tool_definitions(
            &crate::config::Config::default(),
            false,
        ))
        .expect("tools");
        assert!(
            tools.contains(
                "End the turn. text is the result card. proof is optional. Leave proof empty when it would only repeat text."
            ),
            "{tools}"
        );
        assert!(!FINISH_LINE.contains("Written by Kyoto"), "{FINISH_LINE}");
    }

    #[test]
    fn a_skill_is_listed_by_name_and_description() {
        let skills = vec![SkillEntry {
            name: "review".into(),
            description: "How to review a change in this repo".into(),
            path: "/skills/review/SKILL.md".into(),
            ..Default::default()
        }];
        let prompt = system_prompt("/w", &skills, None, "", None);
        assert!(prompt.contains("review"), "the name: {prompt}");
        assert!(
            prompt.contains("How to review a change in this repo"),
            "the description: {prompt}"
        );
        assert!(
            prompt.contains("<skills_instructions>"),
            "the catalog tag: {prompt}"
        );
        assert!(
            prompt.contains("### Available skills"),
            "the catalog heading: {prompt}"
        );
        assert!(
            prompt.contains(
                "- review: How to review a change in this repo (file: /skills/review/SKILL.md)"
            ),
            "the path: {prompt}"
        );
        assert!(prompt.contains("</skills_instructions>"), "{prompt}");
    }

    #[test]
    fn an_empty_description_keeps_the_file_locator() {
        let skills = vec![SkillEntry {
            name: "review".into(),
            path: "/skills/review/SKILL.md".into(),
            ..Default::default()
        }];
        let block = skills_catalog(&skills, None);
        assert!(
            block.contains("- review: (file: /skills/review/SKILL.md)\n"),
            "{block}"
        );
    }

    #[test]
    fn a_skill_with_no_path_omits_the_locator() {
        let skills = vec![SkillEntry {
            name: "review".into(),
            description: "How to review".into(),
            ..Default::default()
        }];
        let block = skills_catalog(&skills, None);
        assert!(block.contains("- review: How to review\n"), "{block}");
        assert!(!block.contains("(file:"), "{block}");
    }

    #[test]
    fn a_skill_body_is_absent_from_the_system_prompt() {
        // The body is a fixture, not something the prompt builder is given. The
        // index entry carries a name and a description and nothing else, so a
        // body cannot reach the prompt. This test pins that.
        let body = "SECRET SKILL BODY THAT MUST NOT APPEAR IN THE PROMPT";
        let skills = vec![SkillEntry {
            name: "review".into(),
            description: "How to review a change".into(),
            ..Default::default()
        }];
        let prompt = system_prompt("/w", &skills, None, "", None);
        assert!(
            !prompt.contains(body),
            "the skill body is absent from the prompt: {prompt}"
        );
    }

    #[test]
    fn the_prompt_discovers_closeout_requirements_through_the_tool() {
        let file = crate::closeout::CloseoutFile {
            imports: Vec::new(),
            reviews: std::collections::HashMap::new(),
            setup: Vec::new(),
            executions: Default::default(),
            items: vec![
                crate::closeout::CloseoutItem {
                    id: "pinned-test-id".into(),
                    kind: crate::closeout::CloseoutKind::Command,
                    run: "cargo test".into(),
                    hint: "Fix the failing test".into(),
                    paths: vec![],
                },
                crate::closeout::CloseoutItem {
                    id: "pinned-lint-id".into(),
                    kind: crate::closeout::CloseoutKind::Command,
                    run: "cargo clippy".into(),
                    hint: "Fix the lint".into(),
                    paths: vec![],
                },
            ],
            max_failures: 3,
            retry: None,
            policy_digest: String::new(),
            policy_files: Default::default(),
        };
        let prompt = system_prompt("/w", &[], Some(&file), "", None);
        assert!(!prompt.contains("pinned-test-id"), "{prompt}");
        assert!(!prompt.contains("pinned-lint-id"), "{prompt}");
        assert!(!prompt.contains("Fix the failing test"), "{prompt}");
        assert!(!prompt.contains("Fix the lint"), "{prompt}");
        assert!(prompt.contains("`get_closeout`"), "{prompt}");
        assert!(
            prompt.contains("`run_closeout`"),
            "the instruction: {prompt}"
        );
        assert!(
            prompt.contains(
                "If a required check fails, fix it and call run_closeout again. If you cannot fix it, call ask with the error and what you tried."
            ),
            "the retry instruction: {prompt}"
        );
    }

    #[test]
    fn a_workspace_with_no_closeout_file_has_no_closeout_section() {
        let prompt = system_prompt("/w", &[], None, "", None);
        assert!(
            !prompt.contains("closeout checks"),
            "no closeout section: {prompt}"
        );
    }

    fn entry(name: &str, description: &str) -> SkillEntry {
        SkillEntry {
            name: name.to_string(),
            description: description.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_catalog_over_two_percent_keeps_every_name_and_shortens_descriptions() {
        let skills = vec![
            entry("alpha", &"a".repeat(500)),
            entry("beta", &"b".repeat(500)),
        ];
        let window = 2_500u64;
        let budget = catalog_char_budget(Some(window));
        assert_eq!(budget, 200);
        let block = skills_catalog(&skills, Some(window));
        assert!(block.chars().count() <= budget, "over the budget: {block}");
        assert!(block.contains("alpha"), "the first name: {block}");
        assert!(block.contains("beta"), "the second name: {block}");
        assert!(
            !block.contains(&"a".repeat(500)),
            "the long description stays"
        );
        assert!(
            block.contains("a"),
            "a shortened description remains: {block}"
        );
        let parts = prompt_parts("/w", &skills, None, "", Some(window));
        assert!(
            !parts.without_skills.contains("alpha"),
            "the catalog is not in the system half: {}",
            parts.without_skills
        );
        assert!(
            !parts.without_skills.contains("<skills_instructions>"),
            "the catalog header is not in the system half"
        );
        assert!(parts.full.contains(&parts.skills));
    }

    #[test]
    fn an_unknown_window_caps_the_catalog_at_eight_thousand_characters() {
        let skills = vec![entry("alpha", &"a".repeat(20_000))];
        let block = skills_catalog(&skills, None);
        assert!(block.chars().count() <= UNKNOWN_CATALOG_CHARS, "{block}");
        assert!(block.contains("alpha"), "{block}");
        assert!(!block.contains(&"a".repeat(8_000)), "{block}");
    }

    #[test]
    fn catalog_fit_names_a_shortened_description_and_a_dropped_name() {
        let short = vec![entry("alpha", "review a change")];
        assert_eq!(catalog_fit(&short, Some(8_000)), CatalogFit::Fits);
        let long = vec![
            entry("alpha", &"a".repeat(500)),
            entry("beta", &"b".repeat(500)),
        ];
        match catalog_fit(&long, Some(8_000)) {
            CatalogFit::DescriptionsShortened { kept } => assert!(kept < 500 && kept > 0, "{kept}"),
            other => panic!("expected shortened descriptions, got {other:?}"),
        }
        let many: Vec<SkillEntry> = (0..40)
            .map(|index| entry(&format!("skill{index:02}"), "desc"))
            .collect();
        match catalog_fit(&many, Some(5_000)) {
            CatalogFit::NamesDropped { listed, omitted } => {
                assert!(listed > 0, "{listed}");
                assert_eq!(listed + omitted, 40);
            }
            other => panic!("expected dropped names, got {other:?}"),
        }
    }

    #[test]
    fn names_that_overflow_the_budget_keep_a_prefix_and_a_trailing_line() {
        let skills: Vec<SkillEntry> = (0..40)
            .map(|index| entry(&format!("skill{index:02}"), "desc"))
            .collect();
        let block = skills_catalog(&skills, Some(5_000));
        assert!(
            block.contains("more skills. Call use_skill by name or the user will type /name."),
            "{block}"
        );
        assert!(block.contains("skill00"), "a prefix name: {block}");
        assert!(!block.contains("skill39"), "the tail name drops: {block}");
        assert!(
            !block.contains(": desc"),
            "descriptions drop before names: {block}"
        );
        let omitted = 40
            - block
                .lines()
                .filter(|line| line.starts_with("- skill"))
                .count();
        assert!(
            block.contains(&format!("{omitted} more skills.")),
            "the count matches the omitted names: {block}"
        );
    }

    #[test]
    fn agents_text_sits_after_the_tools_and_before_the_skills() {
        let skills = vec![SkillEntry {
            name: "review".into(),
            description: "How to review a change".into(),
            ..Default::default()
        }];
        let prompt = system_prompt("/w", &skills, None, "pnpm test\n", None);
        let identity = prompt.find("You are Kyoto Agent").expect("identity");
        let agents_at = prompt
            .find("Project instructions (AGENTS.md):")
            .expect("agents");
        let body_at = prompt.find("pnpm test").expect("the body");
        let skills_at = prompt.find("<skills_instructions>").expect("skills");
        assert!(identity < agents_at && agents_at < body_at && body_at < skills_at);
        assert!(
            !prompt.contains("SECRET SKILL BODY"),
            "a body the builder was not given stays out"
        );
    }

    #[test]
    fn an_empty_agents_string_omits_the_section() {
        let prompt = system_prompt("/w", &[], None, "", None);
        assert!(!prompt.contains("Project instructions"), "{prompt}");
    }

    #[test]
    fn parents_and_children_keep_verification_out_of_operator_configuration() {
        for prompt in [
            system_prompt("/w", &[], None, "", None),
            subagent_prompt("/w", &[], None, "", None),
        ] {
            assert!(prompt.contains("Never edit, replace, delete"));
            assert!(prompt.contains("including explicitly requested configuration edits"));
            assert!(prompt.contains("fresh temporary directory for each run"));
            assert!(prompt.contains("Never inherit the operator's KYOTOAGENT_ROOT"));
            assert!(prompt.contains("Never stop, restart, kill, replace, or reconfigure"));
            assert!(prompt.contains("Never use broad process matching to stop servers"));
        }
    }

    #[test]
    fn the_parent_prompt_tells_the_model_to_spawn_near_the_start() {
        let prompt = system_prompt("/w", &[], None, "", None);
        assert!(
            prompt.contains("call `spawn_subagent` near the start"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Call `check_task` with every id"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Leave `run_in_background` true"),
            "{prompt}"
        );
        assert!(prompt.contains("call `kill_task` with its id"), "{prompt}");
        assert!(
            prompt.contains("`archive_session` only archives"),
            "{prompt}"
        );
        assert!(prompt.contains("A child is hidden"), "{prompt}");
        assert!(
            prompt.contains("Clean up every child you started"),
            "{prompt}"
        );
    }

    #[test]
    fn the_subagent_prompt_is_the_short_identity() {
        let file = crate::closeout::CloseoutFile {
            imports: Vec::new(),
            reviews: std::collections::HashMap::new(),
            setup: Vec::new(),
            executions: Default::default(),
            items: vec![crate::closeout::CloseoutItem {
                id: "test".into(),
                kind: crate::closeout::CloseoutKind::Command,
                run: "cargo test".into(),
                hint: "Fix the failing test".into(),
                paths: vec![],
            }],
            max_failures: 3,
            retry: None,
            policy_digest: String::new(),
            policy_files: Default::default(),
        };
        let skills = vec![SkillEntry {
            name: "review".into(),
            description: "How to review a change".into(),
            ..Default::default()
        }];
        let prompt = subagent_prompt("/w", &skills, Some(&file), "pnpm test\n", None);
        assert!(
            prompt.starts_with("You are a Kyoto Agent subagent."),
            "{prompt}"
        );
        assert!(prompt.contains("The workspace is /w."), "{prompt}");
        assert!(prompt.contains("pnpm test"), "{prompt}");
        assert!(prompt.contains("review"), "{prompt}");
        assert!(!prompt.contains("Fix the failing test"), "{prompt}");
        assert!(prompt.contains("`get_closeout`"), "{prompt}");
        assert!(prompt.contains("You work for the parent agent"), "{prompt}");
        assert!(
            prompt.contains(
                "Call `finish` with `text` when the task is done. Leave `proof` empty unless it adds evidence `text` does not already state."
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("Leave `proof` empty when it would only repeat `text`."),
            "{prompt}"
        );
        assert!(!prompt.contains("call `ask`"), "{prompt}");
        assert!(
            prompt.contains(
                "If you cannot fix a required check, call finish with the error and what you tried."
            ),
            "{prompt}"
        );
        assert!(!prompt.contains("You are kyotoagent"), "{prompt}");
        assert!(
            !prompt.contains("You are a kyotoagent subagent"),
            "{prompt}"
        );
        assert!(!prompt.contains("Keep the todo list current"), "{prompt}");
        assert!(!prompt.contains("spawn_subagent"), "{prompt}");
        let identity = prompt
            .find("You are a Kyoto Agent subagent.")
            .expect("identity");
        let work = prompt
            .find("You work for the parent agent")
            .expect("the parent line");
        let agents_at = prompt
            .find("Project instructions (AGENTS.md):")
            .expect("agents");
        let skills_at = prompt.find("<skills_instructions>").expect("skills");
        let closeout_at = prompt.find("closeout checks").expect("closeout");
        assert!(
            identity < work && work < agents_at && agents_at < skills_at && skills_at < closeout_at
        );
    }
}
