pub(super) struct HelpSections {
    pub(super) description_fallback: Vec<String>,
    pub(super) options: Vec<OptionItem>,
    pub(super) exit_status: Vec<String>,
    pub(super) notes: Vec<String>,
}

pub(super) struct CommandEntry {
    pub(super) name: String,
    pub(super) description: Option<String>,
}

pub(super) enum OptionItem {
    Heading(String),
    Option(OptionEntry),
}

pub(super) struct OptionEntry {
    pub(super) names: String,
    pub(super) desc: String,
}
