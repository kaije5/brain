p = 'apps/brain/src/tui/mod.rs'
s = open(p, encoding='utf8').read()

# 1. App state: active model + available models
old = '''    tasks: Vec<(String, String)>,
    notes_query: String,'''
new = '''    tasks: Vec<(String, String)>,
    /// The model the agent currently resolves to (SCRUM-83).
    active_model: Option<String>,
    /// Models offered by routing for mid-session selection.
    available_models: Vec<String>,
    notes_query: String,'''
assert old in s, 'App fields'
s = s.replace(old, new)

old = '''            tasks: Vec::new(),
            notes_query: String::new(),'''
new = '''            tasks: Vec::new(),
            active_model: None,
            available_models: Vec::new(),
            notes_query: String::new(),'''
assert old in s, 'App default'
s = s.replace(old, new)

# 2. InputCommand + submit_input; submit_prompt delegates
old = '''    /// Stages the current input as a user prompt. The runner performs the
    /// policy-checked `cortex_agent_run` request through the daemon.
    pub fn submit_prompt(&mut self) {
        if self.chat_status == ChatStatus::Waiting {
            return;
        }
        let prompt = self.chat_input.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        self.chat_input.clear();
        self.transcript.push(("user".to_owned(), prompt));
        self.chat_status = ChatStatus::Waiting;
    }'''
new = '''    /// Stages the current input as a user prompt. The runner performs the
    /// policy-checked `cortex_agent_run` request through the daemon.
    pub fn submit_prompt(&mut self) {
        let _ = self.submit_input();
    }

    /// Classifies the staged input and stages it. `/model` interactions are
    /// handled without entering the waiting state: an in-flight generation
    /// cannot be interrupted by a selection attempt (SCRUM-83).
    ///
    /// Returns the command the runner should perform, or `None` when the
    /// input was rejected (in-flight generation) or empty.
    pub fn submit_input(&mut self) -> Option<InputCommand> {
        if self.chat_status == ChatStatus::Waiting {
            return None;
        }
        let input = self.chat_input.trim().to_owned();
        if input.is_empty() {
            return None;
        }
        self.chat_input.clear();
        if input == "/model" {
            self.transcript.push(("user".to_owned(), input));
            return Some(InputCommand::ModelList);
        }
        if let Some(model) = input.strip_prefix("/model ") {
            let model = model.trim().to_owned();
            if model.is_empty() {
                self.transcript.push(("user".to_owned(), "/model".to_owned()));
                return Some(InputCommand::ModelList);
            }
            self.transcript.push(("user".to_owned(), input));
            return Some(InputCommand::ModelSelect(model));
        }
        self.transcript.push(("user".to_owned(), input));
        self.chat_status = ChatStatus::Waiting;
        Some(InputCommand::Agent(self.pending_prompt().unwrap_or_default()))
    }

    /// Records the active model and the models offered by routing.
    pub fn set_model_catalog(&mut self, active: Option<String>, models: Vec<String>) {
        self.active_model = active;
        self.available_models = models;
    }

    #[must_use]
    pub fn active_model(&self) -> Option<&str> {
        self.active_model.as_deref()
    }

    #[must_use]
    pub fn available_models(&self) -> &[String] {
        &self.available_models
    }

    /// Applies a successful mid-session model switch.
    pub fn apply_model_switch(&mut self, model: String) {
        self.active_model = Some(model);
    }

    /// Renders an assistant note into the transcript (model listings,
    /// selection confirmations, and actionable errors).
    pub fn push_assistant_note(&mut self, note: String) {
        self.transcript.push(("assistant".to_owned(), note));
    }'''
assert old in s, 'submit anchor'
s = s.replace(old, new)

open(p, 'w', encoding='utf8', newline='\n').write(s)
print('mod ok')
