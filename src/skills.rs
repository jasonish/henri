// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Skills system for Henri.
//!
//! Skills are packaged prompt/command bundles that extend Henri's capabilities.
//! Each skill is a directory containing a `SKILL.md` file with:
//! - YAML/TOML front-matter (name, description)
//! - Body content (prompt instructions injected into system prompt)
//!
//! Search paths (in order):
//! 1. `.henri/skills/` (project-local)
//! 2. `~/.config/henri/skills/` (user)

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use crate::config;

/// A loaded skill.
#[derive(Debug, Clone)]
pub(crate) struct Skill {
    /// Unique identifier (directory name)
    pub _id: String,
    /// Display name from front-matter (or id if not specified)
    pub name: String,
    /// Short description from front-matter
    pub description: String,
    /// Absolute path to the SKILL.md file
    pub location: PathBuf,
    /// Source label for display (e.g., "(project)" or "(user)")
    pub source: String,
}

#[derive(Debug, Clone, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// Load all available skills from search paths.
/// Skills from project directory take precedence over user directory.
pub(crate) fn load_skills() -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Define skill directories to check with their labels
    let mut search_dirs = Vec::new();

    // 1. Project-local: .henri/skills/
    search_dirs.push((Path::new(".henri/skills").to_path_buf(), "(project)"));

    // 2. User config: ~/.config/henri/skills/
    let mut config_skills_dir = config::config_dir();
    config_skills_dir.push("skills");
    search_dirs.push((config_skills_dir, "(user)"));

    for (skills_dir, label) in search_dirs {
        if !skills_dir.exists() || !skills_dir.is_dir() {
            continue;
        }

        // Each subdirectory is a skill
        let Ok(entries) = fs::read_dir(&skills_dir) else {
            continue;
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }

            // Extract skill ID from directory name
            let Some(id) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                continue;
            };

            // Skip if we've already seen this skill ID (project takes precedence)
            if seen_ids.contains(&id) {
                continue;
            }

            // Load and parse the skill file
            let Ok(content) = fs::read_to_string(&skill_file) else {
                continue;
            };

            if let Some(skill) = parse_skill(&id, &content, &skill_file, label) {
                seen_ids.insert(id);
                skills.push(skill);
            }
        }
    }

    // Sort by name for consistent ordering
    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    skills
}

/// Parse a SKILL.md file into a Skill struct.
fn parse_skill(id: &str, content: &str, skill_file: &Path, source: &str) -> Option<Skill> {
    let content = content.trim();

    let (frontmatter, _body) =
        parse_frontmatter(content).unwrap_or((SkillFrontmatter::default(), content));

    let name = frontmatter.name.unwrap_or_else(|| id.to_string());
    let description = frontmatter.description.unwrap_or_default();

    // Get absolute path to the SKILL.md file
    let location = skill_file
        .canonicalize()
        .unwrap_or_else(|_| skill_file.to_path_buf());

    Some(Skill {
        _id: id.to_string(),
        name,
        description,
        location,
        source: source.to_string(),
    })
}

/// Parse front-matter from content.
/// Returns front-matter and body content without front-matter.
fn parse_frontmatter(content: &str) -> Option<(SkillFrontmatter, &str)> {
    let content = content.trim_start();

    // YAML front-matter (---...---)
    if let Some(after_open) = content.strip_prefix("---\n") {
        // Handle empty front-matter (---\n---\n)
        if let Some(body) = after_open.strip_prefix("---\n") {
            return Some((SkillFrontmatter::default(), body.trim_start()));
        }
        // Handle non-empty front-matter
        if let Some(end_pos) = after_open.find("\n---\n") {
            let frontmatter_str = &after_open[..end_pos];
            let body = after_open[end_pos + 5..].trim_start();

            let frontmatter = extract_frontmatter_from_yaml(frontmatter_str);
            return Some((frontmatter, body));
        }
    }

    // TOML front-matter (+++...+++)
    if let Some(after_open) = content.strip_prefix("+++\n") {
        // Handle empty front-matter (+++\n+++\n)
        if let Some(body) = after_open.strip_prefix("+++\n") {
            return Some((SkillFrontmatter::default(), body.trim_start()));
        }
        // Handle non-empty front-matter
        if let Some(end_pos) = after_open.find("\n+++\n") {
            let frontmatter_str = &after_open[..end_pos];
            let body = after_open[end_pos + 5..].trim_start();

            let frontmatter = extract_frontmatter_from_toml(frontmatter_str);
            return Some((frontmatter, body));
        }
    }

    None
}

/// Extract fields from YAML front-matter.
fn extract_frontmatter_from_yaml(yaml: &str) -> SkillFrontmatter {
    let mut frontmatter = SkillFrontmatter::default();

    if let Ok(value) = serde_yaml_ng::from_str::<HashMap<String, JsonValue>>(yaml) {
        frontmatter.name = value
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        frontmatter.description = value
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    frontmatter
}

/// Extract fields from TOML front-matter.
fn extract_frontmatter_from_toml(toml_str: &str) -> SkillFrontmatter {
    let mut frontmatter = SkillFrontmatter::default();

    if let Ok(value) = toml::from_str::<HashMap<String, toml::Value>>(toml_str) {
        frontmatter.name = value
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        frontmatter.description = value
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    frontmatter
}

/// Generate the available_skills prompt block following the agentskills.io format.
/// Returns None if there are no skills, otherwise returns a single formatted block.
/// The model should read the SKILL.md file (via file_read or cat) to activate a skill.
pub(crate) fn get_skill_prompts() -> Option<String> {
    let skills = load_skills();

    if skills.is_empty() {
        return None;
    }

    let mut xml = String::from("<available_skills>\n");

    for skill in skills {
        xml.push_str("  <skill>\n");
        xml.push_str(&format!("    <name>{}</name>\n", skill.name));
        xml.push_str(&format!(
            "    <description>{}</description>\n",
            skill.description
        ));
        xml.push_str(&format!(
            "    <location>{}</location>\n",
            skill.location.display()
        ));
        xml.push_str("  </skill>\n");
    }

    xml.push_str("</available_skills>");

    Some(xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_yaml_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill for testing
---

# Test Skill

This is the prompt content."#;

        let (frontmatter, body) = parse_frontmatter(content).unwrap();
        assert_eq!(frontmatter.name, Some("test-skill".to_string()));
        assert_eq!(
            frontmatter.description,
            Some("A test skill for testing".to_string())
        );
        assert!(body.starts_with("# Test Skill"));
    }

    #[test]
    fn test_parse_toml_frontmatter() {
        let content = r#"+++
name = "test-skill"
description = "A test skill for testing"
+++

# Test Skill

This is the prompt content."#;

        let (frontmatter, body) = parse_frontmatter(content).unwrap();
        assert_eq!(frontmatter.name, Some("test-skill".to_string()));
        assert_eq!(
            frontmatter.description,
            Some("A test skill for testing".to_string())
        );
        assert!(body.starts_with("# Test Skill"));
    }

    #[test]
    fn test_parse_skill_stores_location() {
        let temp_dir = TempDir::new().unwrap();
        let skill_file = temp_dir.path().join("SKILL.md");
        let content = r#"---
name: test
description: Test skill
---

Run: {baseDir}/script.js"#;

        fs::write(&skill_file, content).unwrap();
        let skill = parse_skill("test", content, &skill_file, "(user)").unwrap();

        assert_eq!(skill.name, "test");
        assert_eq!(skill.description, "Test skill");
        assert_eq!(skill.location, skill_file.canonicalize().unwrap());
    }

    #[test]
    fn test_load_skills_from_directory() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        let skill_dir = skills_dir.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();

        let skill_content = r#"---
name: My Skill
description: Does something useful
---

# My Skill

Use {baseDir}/tool.sh to do things."#;

        let skill_file = skill_dir.join("SKILL.md");
        fs::write(&skill_file, skill_content).unwrap();

        // Manually parse since we can't override the search paths easily
        let content = fs::read_to_string(&skill_file).unwrap();
        let skill = parse_skill("my-skill", &content, &skill_file, "(test)").unwrap();

        assert_eq!(skill._id, "my-skill");
        assert_eq!(skill.name, "My Skill");
        assert_eq!(skill.description, "Does something useful");
        assert_eq!(skill.location, skill_file.canonicalize().unwrap());
    }

    #[test]
    fn test_skill_without_frontmatter() {
        let temp_dir = TempDir::new().unwrap();
        let skill_file = temp_dir.path().join("SKILL.md");
        let content = "# Simple Skill\n\nJust some instructions.";

        fs::write(&skill_file, content).unwrap();
        let skill = parse_skill("simple", content, &skill_file, "(user)").unwrap();

        assert_eq!(skill._id, "simple");
        assert_eq!(skill.name, "simple"); // Falls back to ID
        assert_eq!(skill.description, "");
        assert_eq!(skill.location, skill_file.canonicalize().unwrap());
    }
}
