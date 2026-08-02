//! Advisor side-review support.
//!
//! The user-visible execution path is the built-in `advisor` subagent profile
//! advertised through the task tool. The shared policy lives in
//! `xai_tool_types::advisor` so the model-facing task tool and the shell
//! coordinator enforce the same config and budget semantics.

#[allow(unused_imports)]
pub(crate) use xai_tool_types::advisor::{
    AdvisorBudget, AdvisorConfig, AdvisorError, AdvisorTranscriptItem, build_advisor_snapshot,
    validate_advisor_call, validate_advisor_subagent_spawn,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.0) };
        }
    }
    fn set_env(name: &'static str, value: &str) -> EnvGuard {
        unsafe { std::env::set_var(name, value) };
        EnvGuard(name)
    }

    #[test]
    fn advisor_snapshot_caps_tool_outputs_and_tail_truncates() {
        let items = vec![
            AdvisorTranscriptItem::user("old context"),
            AdvisorTranscriptItem::tool("read_file", "x".repeat(3_000)),
            AdvisorTranscriptItem::assistant("recent conclusion"),
        ];

        let snapshot = build_advisor_snapshot(&items, 600);

        assert!(snapshot.starts_with("[…earlier transcript elided…]"));
        assert!(snapshot.contains("recent conclusion"));
        assert!(snapshot.contains("truncated"));
        assert!(snapshot.chars().count() <= 600);
    }

    #[test]
    #[serial]
    fn advisor_config_env_disable_enable_and_budget() {
        let _a = set_env("GROK_ADVISOR_DISABLED", "1");
        assert!(!AdvisorConfig::from_env().enabled);
        drop(_a);
        let _b = set_env("GROK_ADVISOR_ENABLED", "false");
        let _c = set_env("GROK_ADVISOR_BUDGET", "123");
        let cfg = AdvisorConfig::from_env();
        assert!(!cfg.enabled);
        assert_eq!(cfg.token_budget, 123);
    }

    #[test]
    fn advisor_budget_exhaustion_is_clear() {
        let cfg = AdvisorConfig {
            enabled: true,
            token_budget: 2,
            max_snapshot_chars: 100,
        };
        let mut budget = AdvisorBudget::new(cfg.token_budget);
        let err = validate_advisor_call(&cfg, &mut budget, "abcd efgh", "question")
            .expect_err("budget should be exhausted");
        assert!(matches!(err, AdvisorError::BudgetExhausted { .. }));
        assert!(err.to_string().contains("remaining"));
    }

    #[test]
    fn advisor_disabled_and_empty_question_errors_are_distinct() {
        let mut budget = AdvisorBudget::new(100);
        let disabled = AdvisorConfig {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            validate_advisor_call(&disabled, &mut budget, "snapshot", "question"),
            Err(AdvisorError::Disabled)
        );
        let enabled = AdvisorConfig::default();
        assert_eq!(
            validate_advisor_call(&enabled, &mut budget, "snapshot", "   "),
            Err(AdvisorError::EmptyQuestion)
        );
    }
}
