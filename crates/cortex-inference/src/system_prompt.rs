use cortex_application::ApplicationError;

pub(crate) const MAX_SYSTEM_SECTION_BYTES: usize = 16 * 1024;

/// Three-tier system prompt (SCRUM-79), modeled on hermes-agent's
/// `agent/system_prompt.py`. The tiers exist to keep the serialized request
/// prefix byte-stable across turns so the upstream prompt cache stays warm:
/// `stable` never changes within a session, `context` changes only per
/// session, and `volatile` may change per turn but is rendered last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemPrompt {
    stable: String,
    context: Option<String>,
    volatile: Option<String>,
}

impl SystemPrompt {
    /// Builds a prompt with only the stable tier.
    ///
    /// # Errors
    /// Returns a validation error when the stable section is blank or
    /// exceeds the per-section byte cap.
    pub fn new(stable: impl Into<String>) -> Result<Self, ApplicationError> {
        Ok(Self {
            stable: validate_section(stable, "system_prompt_stable")?,
            context: None,
            volatile: None,
        })
    }

    /// Adds the per-session context tier.
    ///
    /// # Errors
    /// Returns a validation error when the section is blank or oversized.
    pub fn with_context(mut self, context: impl Into<String>) -> Result<Self, ApplicationError> {
        self.context = Some(validate_section(context, "system_prompt_context")?);
        Ok(self)
    }

    /// Adds the per-turn volatile tier.
    ///
    /// # Errors
    /// Returns a validation error when the section is blank or oversized.
    pub fn with_volatile(mut self, volatile: impl Into<String>) -> Result<Self, ApplicationError> {
        self.volatile = Some(validate_section(volatile, "system_prompt_volatile")?);
        Ok(self)
    }

    /// The stable prefix alone: the only part upstream prompt caches can
    /// safely key on.
    #[must_use]
    pub fn stable_prefix(&self) -> &str {
        &self.stable
    }

    /// Full render in cache-safe tier order.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = String::from(&self.stable);
        if let Some(context) = &self.context {
            rendered.push_str("\n\n");
            rendered.push_str(context);
        }
        if let Some(volatile) = &self.volatile {
            rendered.push_str("\n\n");
            rendered.push_str(volatile);
        }
        rendered
    }
}

fn validate_section(
    section: impl Into<String>,
    field: &'static str,
) -> Result<String, ApplicationError> {
    let section = section.into();
    if section.trim().is_empty() || section.len() > MAX_SYSTEM_SECTION_BYTES {
        return Err(ApplicationError::Validation { field });
    }
    Ok(section)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tiers_in_cache_safe_order() {
        let prompt = SystemPrompt::new("stable instructions")
            .expect("stable tier is valid")
            .with_context("session context")
            .expect("context tier is valid")
            .with_volatile("turn state")
            .expect("volatile tier is valid");

        assert_eq!(prompt.stable_prefix(), "stable instructions");
        assert_eq!(
            prompt.render(),
            "stable instructions\n\nsession context\n\nturn state"
        );
    }

    #[test]
    fn blank_or_oversized_sections_are_rejected() {
        assert!(SystemPrompt::new("   ").is_err());
        assert!(SystemPrompt::new("x".repeat(MAX_SYSTEM_SECTION_BYTES + 1)).is_err());
        let prompt = SystemPrompt::new("stable").expect("stable tier is valid");
        assert!(prompt.clone().with_context(String::new()).is_err());
        assert!(
            prompt
                .with_volatile("x".repeat(MAX_SYSTEM_SECTION_BYTES + 1))
                .is_err()
        );
    }
}

/// Ordered Stable-tier prompt sources (SCRUM-147).
///
/// The effective Stable tier composes deterministically from, in order:
/// Cortex's protected instructions (mandatory, never user-removable), the
/// operator's global Brain prompt, and the resolved profile's instructions.
/// Composition is a pure function of the configuration: an unchanged
/// configuration renders a byte-identical Stable prefix (SCRUM-79), and
/// nothing outside this configuration — vault text, memories, tool results,
/// other provider content — can enter the tier.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptLayers {
    cortex_protected: String,
    user_global: Option<String>,
    profile: Option<String>,
}

impl PromptLayers {
    /// Builds the layers over Cortex's protected instructions.
    ///
    /// # Errors
    /// Returns a validation error when the protected section is invalid.
    pub fn new(cortex_protected: impl Into<String>) -> Result<Self, ApplicationError> {
        Ok(Self {
            cortex_protected: validate_section(cortex_protected, "system_prompt_protected")?,
            user_global: None,
            profile: None,
        })
    }

    /// Sets the operator's global Brain instruction block.
    ///
    /// # Errors
    /// Returns a validation error when the section is non-empty but invalid
    /// (blank or oversized). `None` and empty strings keep the layer absent.
    pub fn with_user_global(
        mut self,
        user_global: Option<impl Into<String>>,
    ) -> Result<Self, ApplicationError> {
        self.user_global = optional_section(user_global, "system_prompt_user_global")?;
        Ok(self)
    }

    /// Sets the resolved profile's instruction block.
    ///
    /// # Errors
    /// Returns a validation error when the section is non-empty but invalid.
    pub fn with_profile(
        mut self,
        profile: Option<impl Into<String>>,
    ) -> Result<Self, ApplicationError> {
        self.profile = optional_section(profile, "system_prompt_profile")?;
        Ok(self)
    }

    /// Composes the deterministic Stable tier: protected instructions first,
    /// then the global Brain prompt, then profile instructions. Blank layers
    /// are omitted, so a configuration without custom prompts composes to
    /// exactly the protected text.
    #[must_use]
    pub fn compose_stable(&self) -> String {
        let mut composed = String::new();
        let layers: [Option<&str>; 3] = [
            Some(self.cortex_protected.as_str()),
            self.user_global.as_deref(),
            self.profile.as_deref(),
        ];
        for layer in layers.into_iter().flatten() {
            if !composed.is_empty() {
                composed.push_str("\n\n");
            }
            composed.push_str(layer);
        }
        composed
    }

    /// Builds the system prompt with this Stable tier.
    ///
    /// # Errors
    /// Returns a validation error when composition fails validation.
    pub fn into_system_prompt(&self) -> Result<SystemPrompt, ApplicationError> {
        SystemPrompt::new(self.compose_stable())
    }
}

fn optional_section(
    section: Option<impl Into<String>>,
    field: &'static str,
) -> Result<Option<String>, ApplicationError> {
    match section {
        None => Ok(None),
        Some(value) => {
            let value = value.into();
            if value.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(validate_section(value, field)?))
            }
        }
    }
}

#[cfg(test)]
mod layered_tests {
    use super::*;

    #[test]
    fn composes_protected_then_global_then_profile_in_order() {
        let layers = PromptLayers::new("protected policy")
            .expect("protected is valid")
            .with_user_global(Some("global brain instructions"))
            .expect("global is valid")
            .with_profile(Some("profile instructions"))
            .expect("profile is valid");
        let prompt = layers.into_system_prompt().expect("composes");
        assert_eq!(
            prompt.stable_prefix(),
            "protected policy\n\nglobal brain instructions\n\nprofile instructions"
        );
    }

    #[test]
    fn protected_instructions_are_always_present_and_first() {
        let layers = PromptLayers::new("protected policy")
            .expect("protected is valid")
            .with_user_global(Some("user text"))
            .expect("global is valid")
            .with_profile(Some("profile text"))
            .expect("profile is valid");
        assert!(layers.compose_stable().starts_with("protected policy"));
        let bare = PromptLayers::new("protected policy").expect("protected is valid");
        assert_eq!(bare.compose_stable(), "protected policy");
    }

    #[test]
    fn blank_layers_are_omitted_so_legacy_configurations_are_behavior_equivalent() {
        let layers = PromptLayers::new("protected policy")
            .expect("protected is valid")
            .with_user_global(Some(String::new()))
            .expect("blank is absent")
            .with_profile(None::<String>)
            .expect("none is absent");
        assert_eq!(layers.compose_stable(), "protected policy");
    }

    #[test]
    fn unchanged_configuration_composes_a_byte_identical_stable_prefix() {
        let compose = || {
            PromptLayers::new("protected policy")
                .expect("protected is valid")
                .with_user_global(Some("global"))
                .expect("global is valid")
                .with_profile(Some("profile"))
                .expect("profile is valid")
                .compose_stable()
        };
        assert_eq!(compose(), compose());
    }

    #[test]
    fn oversized_layers_are_rejected() {
        let oversized = "x".repeat(MAX_SYSTEM_SECTION_BYTES + 1);
        assert!(
            PromptLayers::new("protected")
                .expect("protected is valid")
                .with_user_global(Some(oversized))
                .is_err()
        );
    }
}
