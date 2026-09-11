use cortex_application::ApplicationError;

const MAX_SYSTEM_SECTION_BYTES: usize = 16 * 1024;

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
