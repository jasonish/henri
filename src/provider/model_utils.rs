// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Utility functions for parsing and manipulating model strings.
//!
//! Models can have variants specified with a `#` suffix (e.g., `claude-3-5-sonnet#thinking`).
//! These functions provide common operations for working with model name/variant pairs.

/// Split a model string into base name and optional variant suffix.
///
/// Models can have variants specified with a `#` separator:
/// - `"claude-3-5-sonnet"` → `("claude-3-5-sonnet", None)`
/// - `"claude-3-5-sonnet#thinking"` → `("claude-3-5-sonnet", Some("thinking"))`
/// - `"claude-opus-4-5-thinking#medium"` → `("claude-opus-4-5-thinking", Some("medium"))`
///
/// # Examples
/// ```ignore
/// let (base, variant) = split_model("claude-3-5-sonnet#thinking");
/// assert_eq!(base, "claude-3-5-sonnet");
/// assert_eq!(variant, Some("thinking"));
/// ```
pub(crate) fn split_model(model: &str) -> (&str, Option<&str>) {
    match model.split_once('#') {
        Some((base, suffix)) if !suffix.is_empty() => (base, Some(suffix)),
        _ => (model, None),
    }
}

/// Get the base model name (without variant suffix) from a model string.
///
/// # Examples
/// ```ignore
/// assert_eq!(base_model_name("claude-3-5-sonnet#thinking"), "claude-3-5-sonnet");
/// assert_eq!(base_model_name("claude-3-5-sonnet"), "claude-3-5-sonnet");
/// ```
pub(crate) fn base_model_name(model: &str) -> &str {
    split_model(model).0
}

/// Get the variant suffix from a model string, if present.
///
/// # Examples
/// ```ignore
/// assert_eq!(model_variant("claude-3-5-sonnet#thinking"), Some("thinking"));
/// assert_eq!(model_variant("claude-3-5-sonnet"), None);
/// ```
pub(crate) fn model_variant(model: &str) -> Option<&str> {
    split_model(model).1
}

/// Get all variants for a given base model from a static model list.
///
/// This filters the provided model list to find all entries that share
/// the same base model name.
pub(crate) fn get_model_variants(base: &str, models: &[&'static str]) -> Vec<&'static str> {
    models
        .iter()
        .filter(|m| base_model_name(m) == base)
        .copied()
        .collect()
}

/// Cycle to the next variant for the given model within a model list.
///
/// Returns the new full model string with the next variant. If the model
/// has no variants in the list, returns the original model string.
///
/// # Arguments
/// * `model` - The current model string (may include variant suffix)
/// * `models` - The list of available models to cycle through
/// * `default_variant` - Optional default variant to use for bare model names
///   (e.g., Some("medium") for thinking models)
pub(crate) fn cycle_model_variant(
    model: &str,
    models: &[&'static str],
    default_variant: Option<&str>,
) -> String {
    let base = base_model_name(model);
    let variants = get_model_variants(base, models);

    if variants.is_empty() {
        return model.to_string();
    }

    // Normalize the model to include the default variant if needed
    let normalized_model = if model_variant(model).is_none() {
        if let Some(default) = default_variant {
            format!("{}#{}", base, default)
        } else {
            model.to_string()
        }
    } else {
        model.to_string()
    };

    // Find current position and cycle to next
    let current_idx = variants
        .iter()
        .position(|v| *v == normalized_model)
        .unwrap_or(0);
    let next_idx = (current_idx + 1) % variants.len();
    variants[next_idx].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_model_with_variant() {
        let (base, variant) = split_model("claude-3-5-sonnet#thinking");
        assert_eq!(base, "claude-3-5-sonnet");
        assert_eq!(variant, Some("thinking"));
    }

    #[test]
    fn test_split_model_without_variant() {
        let (base, variant) = split_model("claude-3-5-sonnet");
        assert_eq!(base, "claude-3-5-sonnet");
        assert_eq!(variant, None);
    }

    #[test]
    fn test_split_model_empty_variant() {
        let (base, variant) = split_model("claude-3-5-sonnet#");
        assert_eq!(base, "claude-3-5-sonnet#");
        assert_eq!(variant, None);
    }

    #[test]
    fn test_base_model_name() {
        assert_eq!(
            base_model_name("claude-3-5-sonnet#thinking"),
            "claude-3-5-sonnet"
        );
        assert_eq!(base_model_name("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_model_variant() {
        assert_eq!(model_variant("model#variant"), Some("variant"));
        assert_eq!(model_variant("model"), None);
    }

    #[test]
    fn test_get_model_variants() {
        const MODELS: &[&str] = &[
            "base-model#off",
            "base-model#medium",
            "base-model#high",
            "other-model",
        ];
        let variants = get_model_variants("base-model", MODELS);
        assert_eq!(
            variants,
            vec!["base-model#off", "base-model#medium", "base-model#high"]
        );
    }

    #[test]
    fn test_cycle_model_variant() {
        const MODELS: &[&str] = &["model#off", "model#medium", "model#high"];

        // Cycle through variants
        assert_eq!(
            cycle_model_variant("model#off", MODELS, None),
            "model#medium"
        );
        assert_eq!(
            cycle_model_variant("model#medium", MODELS, None),
            "model#high"
        );
        assert_eq!(cycle_model_variant("model#high", MODELS, None), "model#off");
    }

    #[test]
    fn test_cycle_model_variant_with_default() {
        const MODELS: &[&str] = &["model#off", "model#medium", "model#high"];

        // Bare model with default should normalize to default variant first
        let result = cycle_model_variant("model", MODELS, Some("medium"));
        assert_eq!(result, "model#high");
    }

    #[test]
    fn test_cycle_model_variant_no_variants() {
        const MODELS: &[&str] = &["other-model"];
        assert_eq!(
            cycle_model_variant("unknown-model", MODELS, None),
            "unknown-model"
        );
    }
}
