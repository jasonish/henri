// SPDX-License-Identifier: MIT
// Model providers and model selection for the TUI

use crate::config::{ConfigFile, DefaultModel};

// Re-export shared types from providers module
pub(crate) use crate::providers::{ModelChoice, ModelProvider, build_model_choices};

/// Parse a model string like "zen/big-pickle" and return a ModelChoice
pub(crate) fn parse_model_string(model_str: &str) -> Option<ModelChoice> {
    let (provider_id, model_id) = model_str.split_once('/')?;

    // Check if it's a custom OpenAI-compatible or Zen provider
    if let Ok(config) = ConfigFile::load()
        && let Some(provider_config) = config.get_provider(provider_id)
    {
        if provider_config.as_openai_compat().is_some() {
            return Some(ModelChoice {
                provider: ModelProvider::OpenAiCompat,
                model_id: model_id.to_string(),
                custom_provider: Some(provider_id.to_string()),
            });
        }
        if provider_config.as_zen().is_some() {
            return Some(ModelChoice {
                provider: ModelProvider::OpenCodeZen,
                model_id: model_id.to_string(),
                custom_provider: Some(provider_id.to_string()),
            });
        }
        if provider_config.as_openrouter().is_some() {
            return Some(ModelChoice {
                provider: ModelProvider::OpenRouter,
                model_id: model_id.to_string(),
                custom_provider: Some(provider_id.to_string()),
            });
        }
    }

    // Otherwise, try to parse as a built-in provider
    let provider = ModelProvider::from_id(provider_id)?;

    Some(ModelChoice {
        provider,
        model_id: model_id.to_string(),
        custom_provider: None,
    })
}

/// Load the default startup model based on config settings.
///
/// Respects the `default-model` config option:
/// - If set to ":last-used", returns the most recently used model
/// - If set to a specific model string, returns that model
pub(crate) fn load_default_model() -> Option<ModelChoice> {
    let config = ConfigFile::load().ok()?;
    let model_str = match &config.default_model {
        DefaultModel::Specific(m) => Some(m.clone()),
        DefaultModel::LastUsed => config.state.as_ref().and_then(|s| s.last_model.clone()),
    }?;
    parse_model_string(&model_str)
}

pub(crate) struct ModelMenuState {
    pub choices: Vec<ModelChoice>,
    pub selected_index: usize,
    pub search_query: String,
}

impl ModelMenuState {
    /// Returns filtered choices matching the search query using fuzzy matching.
    /// Results are sorted by match quality (exact > prefix > fuzzy).
    pub(crate) fn filtered_choices(&self) -> Vec<&ModelChoice> {
        if self.search_query.is_empty() {
            self.choices.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            let mut matches: Vec<(&ModelChoice, u32)> = self
                .choices
                .iter()
                .filter_map(|c| {
                    let mut targets = vec![c.model_id.to_lowercase()];

                    // If there's a custom provider name, use that
                    if let Some(custom) = &c.custom_provider {
                        targets.push(custom.to_lowercase());
                    } else {
                        // Otherwise use the provider's display name and ID
                        targets.push(c.provider.display_name().to_lowercase());
                        targets.push(c.provider.id().to_lowercase());
                    }

                    targets
                        .iter()
                        .filter_map(|t| fuzzy_match_score(&query, t))
                        .min()
                        .map(|score| (c, score))
                })
                .collect();
            matches.sort_by_key(|(_, score)| *score);
            matches.into_iter().map(|(c, _)| c).collect()
        }
    }
}

/// Fuzzy match: checks if all characters of `query` appear in `target` in order.
/// Returns a score (lower is better) or None if no match.
/// Scoring: 0 = exact, 1 = prefix, 2+ = fuzzy (based on gaps between matches).
pub(crate) fn fuzzy_match_score(query: &str, target: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    if target.contains(query) {
        // Exact substring match
        if target == query {
            return Some(0); // Exact match
        }
        if target.starts_with(query) {
            return Some(1); // Prefix match
        }
        return Some(2); // Substring match
    }

    // Fuzzy matching: all query chars must appear in order
    let mut query_chars = query.chars().peekable();
    let mut score: u32 = 10; // Base score for fuzzy matches
    let mut last_match_pos: Option<usize> = None;

    for (pos, tc) in target.chars().enumerate() {
        if let Some(&qc) = query_chars.peek()
            && tc == qc
        {
            // Penalize gaps between consecutive matches
            if let Some(last) = last_match_pos {
                let gap = pos - last - 1;
                score += gap as u32;
            }
            last_match_pos = Some(pos);
            query_chars.next();
        }
    }

    if query_chars.peek().is_none() {
        Some(score)
    } else {
        None // Not all query characters found
    }
}

pub(crate) const MODEL_MENU_MAX_VISIBLE: usize = 12;
pub(crate) const HISTORY_SEARCH_MAX_VISIBLE: usize = 10;

pub(crate) struct HistorySearchState {
    pub search_query: String,
    pub selected_index: usize,
    pub filtered_indices: Vec<usize>,
}

impl HistorySearchState {
    pub(crate) fn new(history_entries: &[String]) -> Self {
        let filtered_indices: Vec<usize> = (0..history_entries.len()).rev().collect();
        Self {
            search_query: String::new(),
            selected_index: 0,
            filtered_indices,
        }
    }

    pub(crate) fn update_filter(&mut self, history_entries: &[String]) {
        if self.search_query.is_empty() {
            self.filtered_indices = (0..history_entries.len()).rev().collect();
        } else {
            let query = self.search_query.to_lowercase();
            let mut matches: Vec<(usize, u32)> = history_entries
                .iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    fuzzy_match_score(&query, &entry.to_lowercase()).map(|score| (idx, score))
                })
                .collect();
            matches.sort_by_key(|(idx, score)| (*score, usize::MAX - *idx));
            self.filtered_indices = matches.into_iter().map(|(idx, _)| idx).collect();
        }
        self.selected_index = 0;
    }

    pub(crate) fn move_up(&mut self) {
        if !self.filtered_indices.is_empty() {
            if self.selected_index > 0 {
                self.selected_index -= 1;
            } else {
                self.selected_index = self.filtered_indices.len().saturating_sub(1);
            }
        }
    }

    pub(crate) fn move_down(&mut self) {
        if !self.filtered_indices.is_empty() {
            if self.selected_index + 1 < self.filtered_indices.len() {
                self.selected_index += 1;
            } else {
                self.selected_index = 0;
            }
        }
    }

    pub(crate) fn selected_entry<'a>(&self, history_entries: &'a [String]) -> Option<&'a String> {
        self.filtered_indices
            .get(self.selected_index)
            .and_then(|&idx| history_entries.get(idx))
    }
}
