// SPDX-License-Identifier: MIT
// Settings menu state for the TUI

use crate::config::{DefaultModel, UiDefault};

use super::models::ModelChoice;

#[derive(Clone, Debug)]
pub(crate) enum SettingOption {
    ShowNetworkStats(bool),
    ShowDiffs(bool),
    /// LSP integration enabled
    LspEnabled(bool),
    /// Todo tools enabled
    TodoEnabled(bool),
    /// Default model setting
    DefaultModel(DefaultModel),
    /// Default UI mode (cli or tui)
    DefaultUi(UiDefault),
}

impl SettingOption {
    pub(crate) fn display(&self) -> String {
        match self {
            SettingOption::ShowNetworkStats(enabled) => format!(
                "Network Stats: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
            SettingOption::ShowDiffs(enabled) => format!(
                "Show Diffs: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
            SettingOption::LspEnabled(enabled) => format!(
                "LSP Integration: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
            SettingOption::TodoEnabled(enabled) => format!(
                "Todo Tools: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
            SettingOption::DefaultModel(dm) => match dm {
                DefaultModel::LastUsed => "Default Model: :last-used".to_string(),
                DefaultModel::Specific(m) => format!("Default Model: {}", m),
            },
            SettingOption::DefaultUi(ui) => match ui {
                UiDefault::Tui => "Default UI: tui".to_string(),
                UiDefault::Cli => "Default UI: cli".to_string(),
            },
        }
    }
}

/// A choice in the default model submenu
#[derive(Clone, Debug)]
pub(crate) enum DefaultModelChoice {
    /// Use the last selected model on startup
    LastUsed,
    /// Use a specific model
    Specific(ModelChoice),
}

impl DefaultModelChoice {
    pub(crate) fn display(&self) -> String {
        match self {
            DefaultModelChoice::LastUsed => ":last-used".to_string(),
            DefaultModelChoice::Specific(model) => model.display(),
        }
    }

    /// Get the display suffix (provider type name in parentheses) for custom providers
    pub(crate) fn display_suffix(&self) -> Option<String> {
        match self {
            DefaultModelChoice::LastUsed => None,
            DefaultModelChoice::Specific(model) => model.display_suffix(),
        }
    }

    /// Convert to DefaultModel for saving to config
    pub(crate) fn to_default_model(&self) -> DefaultModel {
        match self {
            DefaultModelChoice::LastUsed => DefaultModel::LastUsed,
            DefaultModelChoice::Specific(model) => DefaultModel::Specific(model.short_display()),
        }
    }
}

/// State for the default model selection submenu
pub(crate) struct DefaultModelMenuState {
    pub choices: Vec<DefaultModelChoice>,
    pub selected_index: usize,
    pub search_query: String,
}

impl DefaultModelMenuState {
    pub(crate) fn new(model_choices: Vec<ModelChoice>, current_default: &DefaultModel) -> Self {
        let mut choices = vec![DefaultModelChoice::LastUsed];
        choices.extend(model_choices.into_iter().map(DefaultModelChoice::Specific));

        // Find the index of the current default model
        let selected_index = match current_default {
            DefaultModel::LastUsed => 0,
            DefaultModel::Specific(model_str) => choices
                .iter()
                .position(|c| match c {
                    DefaultModelChoice::Specific(m) => m.short_display() == *model_str,
                    _ => false,
                })
                .unwrap_or(0),
        };

        Self {
            choices,
            selected_index,
            search_query: String::new(),
        }
    }

    /// Returns filtered choices matching the search query
    pub(crate) fn filtered_choices(&self) -> Vec<(usize, &DefaultModelChoice)> {
        if self.search_query.is_empty() {
            self.choices.iter().enumerate().collect()
        } else {
            let query = self.search_query.to_lowercase();
            self.choices
                .iter()
                .enumerate()
                .filter(|(_, c)| c.display().to_lowercase().contains(&query))
                .collect()
        }
    }
}

pub(crate) struct SettingsMenuState {
    pub options: Vec<SettingOption>,
    pub selected_index: usize,
    /// Submenu for selecting default model (None = closed, Some = open)
    pub default_model_submenu: Option<DefaultModelMenuState>,
}

impl SettingsMenuState {
    pub(crate) fn new(
        show_network_stats: bool,
        show_diffs: bool,
        lsp_enabled: bool,
        todo_enabled: bool,
        default_model: DefaultModel,
        default_ui: UiDefault,
    ) -> Self {
        Self {
            options: vec![
                SettingOption::ShowNetworkStats(show_network_stats),
                SettingOption::ShowDiffs(show_diffs),
                SettingOption::LspEnabled(lsp_enabled),
                SettingOption::TodoEnabled(todo_enabled),
                SettingOption::DefaultModel(default_model),
                SettingOption::DefaultUi(default_ui),
            ],
            selected_index: 0,
            default_model_submenu: None,
        }
    }

    pub(crate) fn is_submenu_open(&self) -> bool {
        self.default_model_submenu.is_some()
    }
}

/// State for the MCP server toggle menu
pub(crate) struct McpMenuState {
    /// List of server statuses
    pub servers: Vec<crate::mcp::McpServerStatus>,
    /// Currently selected server index
    pub selected_index: usize,
    /// Whether an operation is in progress (starting/stopping a server)
    pub is_loading: bool,
}

impl McpMenuState {
    pub(crate) fn new(servers: Vec<crate::mcp::McpServerStatus>) -> Self {
        Self {
            servers,
            selected_index: 0,
            is_loading: false,
        }
    }
}

/// A tool entry for the tools menu
pub(crate) struct ToolEntry {
    /// Tool name (e.g., "bash", "file_read")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Whether the tool is enabled (not in disabled_tools list)
    pub is_enabled: bool,
}

/// State for the tools toggle menu
pub(crate) struct ToolsMenuState {
    /// List of tool entries
    pub tools: Vec<ToolEntry>,
    /// Currently selected tool index
    pub selected_index: usize,
}

impl ToolsMenuState {
    pub(crate) fn new() -> Self {
        let config = crate::config::ConfigFile::load().unwrap_or_default();
        let disabled_tools = &config.disabled_tools;

        let tools = crate::tools::TOOL_INFO
            .iter()
            .map(|(name, description)| ToolEntry {
                name: name.to_string(),
                description: description.to_string(),
                is_enabled: !disabled_tools.iter().any(|t| t == *name),
            })
            .collect();

        Self {
            tools,
            selected_index: 0,
        }
    }

    /// Toggle the selected tool's enabled status
    pub(crate) fn toggle_selected(&mut self) -> Option<(&str, bool)> {
        if let Some(tool) = self.tools.get_mut(self.selected_index) {
            tool.is_enabled = !tool.is_enabled;

            // Update config
            if let Ok(mut config) = crate::config::ConfigFile::load() {
                config.toggle_tool_disabled(&tool.name);
                let _ = config.save();
            }

            Some((&tool.name, tool.is_enabled))
        } else {
            None
        }
    }
}
