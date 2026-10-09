use serde::{Deserialize, Serialize};

pub(crate) const MAX_TOOL_CALLS_PER_RESUME: u32 = 100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Complete,
    BudgetExhausted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    #[serde(default)]
    pub id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub rounds: u64,
    pub verification: String,
    pub evidence: Vec<GoalEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<GoalCriterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<GoalEvaluation>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub evaluations_since_resume: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blocked_streak: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repairs_since_resume: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeated_gap: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_calls_since_resume: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub evaluations_since_checkpoint: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<GoalCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<GoalRecovery>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalCriterion {
    pub outcome: String,
    pub verification: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDecision {
    Continue,
    CandidateComplete,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalEvaluation {
    pub decision: GoalDecision,
    pub evidence: String,
    pub next_step: String,
    pub blocker_key: String,
}

impl GoalEvaluation {
    pub fn parse(text: &str) -> Result<Self, String> {
        let verdict: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        if !valid_text(&verdict.evidence) || !valid_text(&verdict.next_step) {
            return Err("Evaluation needs concrete evidence and a next step.".into());
        }
        let key = &verdict.blocker_key;
        if (verdict.decision == GoalDecision::Blocked
            && (key.is_empty()
                || key.len() > 120
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')))
            || (verdict.decision != GoalDecision::Blocked && !key.is_empty())
        {
            return Err("Only blocked evaluations need a lowercase snake_case blocker_key.".into());
        }
        Ok(verdict)
    }
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 4000
}

pub fn parse_criteria(text: &str) -> Result<Vec<GoalCriterion>, String> {
    let criteria: Vec<GoalCriterion> =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    if criteria.is_empty()
        || criteria.len() > 5
        || criteria.iter().any(|criterion| {
            !valid_text(&criterion.outcome) || !valid_text(&criterion.verification)
        })
    {
        return Err(
            "Goal plan needs one to five outcomes, each with a verification procedure.".into(),
        );
    }
    Ok(criteria)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalWorkItem {
    pub work_item: String,
    pub evidence_refs: Vec<String>,
    pub blocker: String,
    pub next_action: String,
}

impl GoalWorkItem {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let item: Self =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        if !valid_text(&item.work_item)
            || !valid_text(&item.next_action)
            || item.blocker.len() > 4000
            || item.evidence_refs.is_empty()
            || item.evidence_refs.len() > 20
            || item
                .evidence_refs
                .iter()
                .any(|reference| !valid_text(reference))
        {
            return Err(
                "Checkpoint needs a work item, evidence references and a next action.".into(),
            );
        }
        Ok(item)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalCheckpoint {
    pub item: GoalWorkItem,
    pub revision: String,
    pub workspace_fingerprint: String,
    pub through_event_id: String,
    pub verification: String,
    pub evidence: Vec<GoalEvidence>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalRecovery {
    pub reason: String,
    pub event_ids: Vec<String>,
    pub interrupted_tasks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalEvidence {
    pub tool: String,
    pub args: serde_json::Value,
    pub output: String,
}

impl Goal {
    pub fn new(objective: &str, token_budget: Option<u64>) -> Self {
        Self {
            id: crate::session::new_task_id(),
            objective: objective.to_string(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            rounds: 0,
            verification: String::new(),
            evidence: Vec::new(),
            criteria: Vec::new(),
            evaluation: None,
            evaluations_since_resume: 0,
            blocked_streak: 0,
            repairs_since_resume: 0,
            repeated_gap: 0,
            tool_calls_since_resume: 0,
            evaluations_since_checkpoint: 0,
            checkpoints: Vec::new(),
            recovery: None,
        }
    }

    pub fn resume(&mut self) {
        self.status = GoalStatus::Active;
        self.evaluations_since_resume = 0;
        self.evaluations_since_checkpoint = 0;
        self.recovery = None;
        self.blocked_streak = 0;
        self.repairs_since_resume = 0;
        self.repeated_gap = 0;
        self.tool_calls_since_resume = 0;
    }

    pub fn begin_tool_call(&mut self) {
        if self.status != GoalStatus::Active {
            return;
        }
        if self.tool_calls_since_resume >= MAX_TOOL_CALLS_PER_RESUME {
            self.pause("Reached the safety limit of 100 tool calls since resume. Inspect the latest command results and remaining work before continuing.");
        } else {
            self.tool_calls_since_resume += 1;
        }
    }

    pub fn evaluate(&mut self, verdict: GoalEvaluation) {
        self.evaluations_since_resume = self.evaluations_since_resume.saturating_add(1);
        self.evaluations_since_checkpoint = self.evaluations_since_checkpoint.saturating_add(1);
        self.blocked_streak = if verdict.decision == GoalDecision::Blocked {
            if self
                .evaluation
                .as_ref()
                .is_some_and(|prior| prior.blocker_key == verdict.blocker_key)
            {
                self.blocked_streak.saturating_add(1)
            } else {
                1
            }
        } else {
            0
        };
        if self.blocked_streak >= 3 {
            self.pause(&format!(
                "{} Next user action: {}",
                verdict.evidence, verdict.next_step
            ));
        } else if self.evaluations_since_resume >= 200 {
            self.pause(
                "Two hundred goal evaluations since resume. Review progress before continuing.",
            );
        } else if self.evaluations_since_checkpoint >= 20 {
            self.pause("Twenty goal evaluations without a new verified checkpoint. Review the remaining work before resuming.");
        }
        self.evaluation = Some(verdict);
    }

    pub fn record_checkpoint(&mut self, checkpoint: GoalCheckpoint) -> bool {
        if self.status != GoalStatus::Active
            || checkpoint.evidence.is_empty()
            || checkpoint.verification.trim().is_empty()
            || checkpoint.workspace_fingerprint.is_empty()
            || self.checkpoints.iter().any(|prior| {
                prior.item.work_item == checkpoint.item.work_item
                    || prior.workspace_fingerprint == checkpoint.workspace_fingerprint
            })
        {
            return false;
        }
        self.checkpoints.push(checkpoint);
        self.evaluations_since_checkpoint = 0;
        self.blocked_streak = 0;
        self.repairs_since_resume = 0;
        self.repeated_gap = 0;
        true
    }

    pub fn reject(&mut self, explanation: &str) {
        self.repairs_since_resume = self.repairs_since_resume.saturating_add(1);
        self.repeated_gap = if self.verification == explanation {
            self.repeated_gap.saturating_add(1)
        } else {
            1
        };
        self.verification = explanation.to_string();
        if self.repeated_gap >= 2 {
            self.pause(&format!(
                "Verification repeated the same gap without progress: {explanation}"
            ));
        } else if self.repairs_since_resume >= 10 {
            self.pause(&format!(
                "Ten unsuccessful verification attempts: {explanation}"
            ));
        }
    }

    pub fn pause(&mut self, reason: &str) {
        self.status = GoalStatus::Paused;
        self.verification = format!("{reason} Use /goal resume to continue.");
    }

    pub fn charge(&mut self, tokens: u64) {
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        if self.status == GoalStatus::Active
            && self
                .token_budget
                .is_some_and(|budget| self.tokens_used >= budget)
        {
            self.status = GoalStatus::BudgetExhausted;
        }
    }

    pub fn checkpoint_context(&self) -> Vec<serde_json::Value> {
        self.checkpoints.iter().map(|checkpoint| serde_json::json!({
            "item": checkpoint.item, "revision": checkpoint.revision,
            "verification": checkpoint.verification, "through_event_id": checkpoint.through_event_id,
        })).collect()
    }

    pub fn summary(&self) -> String {
        format!(
            "Goal {:?}: {}\nRounds: {}. Tokens used: {}.{}{}",
            self.status,
            self.objective,
            self.rounds,
            self.tokens_used,
            self.token_budget
                .map(|budget| format!(" Budget: {budget}."))
                .unwrap_or_default(),
            if self.verification.is_empty() {
                String::new()
            } else {
                format!("\n{}", self.verification)
            }
        ) + &self
            .criteria
            .iter()
            .enumerate()
            .map(|(index, criterion)| {
                format!(
                    "\n{}. {} — {}",
                    index + 1,
                    criterion.outcome,
                    criterion.verification
                )
            })
            .collect::<String>()
            + &self.checkpoints.last().map(|checkpoint| format!(
                "\nVerified checkpoint: {} @ {}\nEvidence: {}\nBlocker: {}\nNext action: {}",
                checkpoint.item.work_item, checkpoint.revision,
                checkpoint.item.evidence_refs.join(", "), checkpoint.item.blocker,
                checkpoint.item.next_action,
            )).unwrap_or_default()
            + &self.recovery.as_ref().map(|recovery| format!(
                "\nRecovery: {}\nReconcile events: {}. Interrupted tasks: {}. Inspect existing external results before retrying any action. After reconciliation, use /goal resume --reconciled.",
                recovery.reason, recovery.event_ids.join(", "), recovery.interrupted_tasks.join(", "),
            )).unwrap_or_default()
    }
}

pub fn parse_objective(text: &str) -> Result<(String, Option<u64>), String> {
    let (objective, budget) = match text.rsplit_once(" --budget ") {
        Some((objective, budget)) => {
            let tokens = budget
                .trim()
                .parse::<u64>()
                .map_err(|_| "Goal budget must be a positive token count.")?;
            if tokens == 0 {
                return Err("Goal budget must be a positive token count.".to_string());
            }
            (objective.trim(), Some(tokens))
        }
        None => (text.trim(), None),
    };
    if objective.is_empty() {
        return Err("Use /goal <objective> [--budget <tokens>].".to_string());
    }
    Ok((objective.to_string(), budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(index: u32) -> GoalCheckpoint {
        GoalCheckpoint {
            item: GoalWorkItem {
                work_item: format!("issue-{index}"),
                evidence_refs: vec!["proof.txt".into()],
                blocker: String::new(),
                next_action: "Next issue".into(),
            },
            revision: format!("revision-{index}"),
            workspace_fingerprint: format!("fingerprint-{index}"),
            through_event_id: format!("e{index}"),
            verification: "Checked actual result".into(),
            evidence: vec![GoalEvidence {
                tool: "run".into(),
                args: serde_json::json!({"argv":["true"]}),
                output: "exited 0".into(),
            }],
        }
    }

    fn continuing() -> GoalEvaluation {
        GoalEvaluation {
            decision: GoalDecision::Continue,
            evidence: "Still working".into(),
            next_step: "Check result".into(),
            blocker_key: String::new(),
        }
    }

    #[test]
    fn verified_progress_can_cross_twenty_evaluations_but_remains_bounded() {
        let mut goal = Goal::new("All issues", None);
        for index in 0..199 {
            goal.evaluate(continuing());
            assert!(goal.record_checkpoint(checkpoint(index)));
        }
        goal.evaluate(continuing());
        assert!(!goal.record_checkpoint(checkpoint(200)));
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.evaluations_since_resume, 200);
        assert_eq!(goal.checkpoints.len(), 199);
    }

    #[test]
    fn repeated_checkpoints_and_unverified_claims_do_not_reset_stalls() {
        let mut goal = Goal::new("All issues", None);
        assert!(goal.record_checkpoint(checkpoint(1)));
        for index in 0..20 {
            goal.evaluate(continuing());
            assert!(!goal.record_checkpoint(checkpoint(1)));
            let mut claim = checkpoint(index + 2);
            claim.evidence.clear();
            assert!(!goal.record_checkpoint(claim));
            let mut churn = checkpoint(index + 2);
            churn.workspace_fingerprint = checkpoint(1).workspace_fingerprint;
            assert!(!goal.record_checkpoint(churn));
        }
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.checkpoints.len(), 1);
        let restored: Goal = serde_json::from_str(&serde_json::to_string(&goal).unwrap()).unwrap();
        assert_eq!(restored, goal);
    }

    #[test]
    fn objective_preserves_spaces_and_reads_a_positive_budget() {
        assert_eq!(
            parse_objective("Migrate the auth module --budget 100"),
            Ok(("Migrate the auth module".to_string(), Some(100)))
        );
        assert_eq!(
            parse_objective("Fix the bug"),
            Ok(("Fix the bug".to_string(), None))
        );
        assert!(parse_objective("").is_err());
        assert!(parse_objective("Fix --budget 0").is_err());
        assert!(parse_objective("Fix --budget nope").is_err());
    }

    #[test]
    fn criteria_and_evaluations_reject_malformed_or_unbounded_contracts() {
        for input in [
            "[]",
            "{}",
            r#"[{"outcome":"","verification":"test"}]"#,
            r#"[{"outcome":"works","verification":"test","extra":true}]"#,
        ] {
            assert!(parse_criteria(input).is_err());
        }
        let criteria = vec![
            GoalCriterion {
                outcome: "works".into(),
                verification: "test".into()
            };
            6
        ];
        assert!(parse_criteria(&serde_json::to_string(&criteria).unwrap()).is_err());
        for (decision, key, valid) in [
            ("continue", "", true),
            ("candidate_complete", "", true),
            ("blocked", "missing_auth", true),
            ("continue", "missing_auth", false),
            ("blocked", "", false),
            ("blocked", "Missing Auth", false),
            ("complete", "", false),
        ] {
            let input = serde_json::json!({"decision": decision, "evidence": "observed", "next_step": "run it", "blocker_key": key});
            assert_eq!(GoalEvaluation::parse(&input.to_string()).is_ok(), valid);
        }
    }

    #[test]
    fn changing_blockers_reset_the_streak_and_continuation_is_bounded() {
        let mut goal = Goal::new("Fix", None);
        for index in 0..20 {
            goal.evaluate(GoalEvaluation {
                decision: GoalDecision::Blocked,
                evidence: "Missing access".into(),
                next_step: "Grant access".into(),
                blocker_key: format!("resource_{index}"),
            });
            assert_eq!(goal.blocked_streak, 1);
        }
        assert_eq!(goal.status, GoalStatus::Paused);
        assert!(goal.verification.contains("Twenty"));
    }

    #[test]
    fn repair_cap_and_resume_preserve_the_goal_and_usage() {
        let mut goal = Goal::new("Fix", Some(100));
        goal.charge(12);
        for index in 0..10 {
            goal.reject(&format!("Gap {index}"));
        }
        assert_eq!(goal.status, GoalStatus::Paused);
        assert!(goal.verification.contains("Ten"));
        let id = goal.id.clone();
        goal.resume();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.id, id);
        assert_eq!(goal.tokens_used, 12);
        assert_eq!(goal.repairs_since_resume, 0);
        assert_eq!(goal.repeated_gap, 0);
    }

    #[test]
    fn tool_call_limit_survives_reload_and_only_resume_resets_it() {
        let mut goal = Goal::new("Fix", None);
        for _ in 0..MAX_TOOL_CALLS_PER_RESUME {
            goal.begin_tool_call();
        }
        assert_eq!(goal.status, GoalStatus::Active);
        let mut goal: Goal = serde_json::from_str(&serde_json::to_string(&goal).unwrap()).unwrap();
        goal.evaluate(GoalEvaluation {
            decision: GoalDecision::Continue,
            evidence: "Still failing".into(),
            next_step: "Inspect the failure".into(),
            blocker_key: String::new(),
        });
        goal.begin_tool_call();
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.tool_calls_since_resume, MAX_TOOL_CALLS_PER_RESUME);
        let id = goal.id.clone();
        goal.resume();
        assert_eq!(goal.id, id);
        assert_eq!(goal.tool_calls_since_resume, 0);
        goal.begin_tool_call();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.tool_calls_since_resume, 1);
    }

    #[test]
    fn old_goal_json_round_trips_without_new_empty_fields() {
        let old = serde_json::json!({"id":"old", "objective":"Fix", "status":"paused", "token_budget":null, "tokens_used":12, "rounds":1, "verification":"Missing proof", "evidence":[]});
        let goal: Goal = serde_json::from_value(old.clone()).unwrap();
        assert!(goal.criteria.is_empty());
        assert_eq!(serde_json::to_value(goal).unwrap(), old);
    }

    #[test]
    fn spending_does_not_reactivate_a_paused_goal() {
        let mut goal = Goal::new("Fix", Some(10));
        goal.status = GoalStatus::Paused;
        goal.charge(20);
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.tokens_used, 20);
    }
}
