//! Persisted idle policy. Loading is all-or-nothing; no automatic action is enabled by default.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    #[default]
    Lock,
    ScreenOff,
    Suspend,
    LockAndSuspend,
    Command,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdleBehavior {
    pub name: String,
    pub enabled: bool,
    pub timeout_seconds: u32,
    pub locked_timeout_seconds: u32,
    pub action: IdleAction,
    pub command: String,
    pub resume_command: String,
    pub lock_before_suspend: bool,
}

impl Default for IdleBehavior {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: false,
            timeout_seconds: 0,
            locked_timeout_seconds: 0,
            action: IdleAction::Lock,
            command: String::new(),
            resume_command: String::new(),
            lock_before_suspend: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdleConfig {
    pub pre_action_fade_seconds: u32,
    pub behaviors: Vec<IdleBehavior>,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            pre_action_fade_seconds: 2,
            behaviors: [
                ("lock", 600, IdleAction::Lock),
                ("screen-off", 660, IdleAction::ScreenOff),
                ("lock-and-suspend", 900, IdleAction::LockAndSuspend),
            ]
            .into_iter()
            .map(|(name, timeout_seconds, action)| IdleBehavior {
                name: name.into(),
                timeout_seconds,
                action,
                ..IdleBehavior::default()
            })
            .collect(),
        }
    }
}

impl IdleConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.pre_action_fade_seconds > 60 {
            return Err("Warning fade must be between 0 and 60 seconds.".into());
        }
        if self.behaviors.len() > 64 {
            return Err("Use at most 64 idle actions.".into());
        }
        let mut names = HashSet::new();
        for behavior in &self.behaviors {
            let name = behavior.name.trim();
            if name.is_empty()
                || behavior.name.len() > 128
                || behavior.name.chars().any(char::is_control)
            {
                return Err(
                    "Each action needs a name of 1–128 bytes without control characters.".into(),
                );
            }
            if !names.insert(name) {
                return Err(format!("Action name {name:?} is used more than once."));
            }
            if behavior.timeout_seconds > 86_400 || behavior.locked_timeout_seconds > 86_400 {
                return Err(format!(
                    "Timeouts for {name:?} must be between 0 and 86400 seconds."
                ));
            }
            if behavior.enabled
                && behavior.action == IdleAction::Command
                && behavior.command.trim().is_empty()
            {
                return Err(format!("Enter a command for {name:?} before enabling it."));
            }
        }
        Ok(())
    }

    pub fn from_json(value: &Value) -> Result<Self, String> {
        // An enabled, incomplete entry must not acquire a default action by accident.
        if let Some(behaviors) = value.get("behaviors").and_then(Value::as_array) {
            for behavior in behaviors {
                if behavior.get("enabled").and_then(Value::as_bool) == Some(true)
                    && behavior.get("action").is_none()
                {
                    return Err("An enabled idle action must explicitly specify its action.".into());
                }
            }
        }
        let config: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("Invalid idle configuration: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("idle configuration contains only JSON-compatible values")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_policy_is_safe_and_invalid_entries_reject_the_whole_policy() {
        let config = IdleConfig::from_json(&json!({})).unwrap();
        assert!(config.behaviors.iter().all(|behavior| !behavior.enabled));
        assert!(
            IdleConfig::from_json(&json!({"behaviors": [
                {"name": "valid", "enabled": true, "action": "lock", "timeout_seconds": 600},
                {"name": "invalid", "enabled": true, "action": "command", "command": " "}
            ]}))
            .is_err()
        );
    }

    #[test]
    fn rejects_unsafe_numeric_and_partial_policies() {
        for timeout in [
            json!(-1),
            json!(1.5),
            json!(86_401),
            json!(u64::MAX),
            json!("600"),
        ] {
            assert!(
                IdleConfig::from_json(&json!({"behaviors": [{
                    "name": "custom", "action": "lock", "enabled": true, "timeout_seconds": timeout
                }]}))
                .is_err()
            );
        }
        assert!(
            IdleConfig::from_json(&json!({"behaviors": [{
                "name": "incomplete", "enabled": true, "timeout_seconds": 1
            }]}))
            .is_err()
        );
        assert!(
            IdleConfig::from_json(&json!({"behaviors": [{
                "name": "typo", "enabled": true, "action": "screen_of"
            }]}))
            .is_err()
        );
        assert!(IdleConfig::from_json(&json!({"pre_action_fade_seconds": 61})).is_err());
    }

    #[test]
    fn validates_names_count_and_enabled_commands() {
        let mut config = IdleConfig {
            behaviors: vec![IdleBehavior {
                name: "custom".into(),
                action: IdleAction::Command,
                ..IdleBehavior::default()
            }],
            ..IdleConfig::default()
        };
        assert!(config.validate().is_ok());
        config.behaviors[0].enabled = true;
        assert!(config.validate().is_err());
        config.behaviors[0].command = "printf ready".into();
        assert!(config.validate().is_ok());
        config.behaviors.push(IdleBehavior {
            name: " custom ".into(),
            ..IdleBehavior::default()
        });
        assert!(config.validate().is_err());
        for name in [
            String::new(),
            " ".into(),
            "x".repeat(129),
            "bad\nname".into(),
        ] {
            config.behaviors = vec![IdleBehavior {
                name,
                ..IdleBehavior::default()
            }];
            assert!(config.validate().is_err());
        }
        config.behaviors = (0..65)
            .map(|i| IdleBehavior {
                name: format!("action-{i}"),
                ..IdleBehavior::default()
            })
            .collect();
        assert!(config.validate().is_err());
    }

    #[test]
    fn roundtrip_preserves_custom_commands_and_separate_clocks() {
        let config = IdleConfig {
            pre_action_fade_seconds: 0,
            behaviors: vec![IdleBehavior {
                name: "custom".into(),
                enabled: true,
                timeout_seconds: 0,
                locked_timeout_seconds: 86_400,
                action: IdleAction::Command,
                command: "printf '%s' 'on idle'".into(),
                resume_command: "printf resume".into(),
                lock_before_suspend: false,
            }],
        };
        assert_eq!(IdleConfig::from_json(&config.to_json()).unwrap(), config);
        assert!(
            IdleConfig::from_json(&json!({"behaviors": []}))
                .unwrap()
                .behaviors
                .is_empty()
        );
    }
}
