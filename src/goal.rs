use serde::{Deserialize, Serialize};

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
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub rounds: u64,
    pub verification: String,
    pub evidence: Vec<GoalEvidence>,
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
            objective: objective.to_string(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            rounds: 0,
            verification: String::new(),
            evidence: Vec::new(),
        }
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
        )
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
    fn spending_does_not_reactivate_a_paused_goal() {
        let mut goal = Goal::new("Fix", Some(10));
        goal.status = GoalStatus::Paused;
        goal.charge(20);
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.tokens_used, 20);
    }
}
