// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Todo list tools for tracking task progress.
//!
//! Provides TodoWrite and TodoRead tools that allow AI to track progress
//! on multi-step tasks with pending/in_progress/completed states.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{Tool, ToolDefinition, ToolResult};
use crate::output;

/// Status of a todo item
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// A single todo item
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct TodoItem {
    /// What needs to be done (imperative form: "Run tests")
    pub content: String,
    /// Current status of the item
    pub status: TodoStatus,
    /// Present continuous form for display ("Running tests")
    pub active_form: String,
}

/// Global todo state
static TODO_STATE: Mutex<Vec<TodoItem>> = Mutex::new(Vec::new());

/// Get a copy of the current todo list
pub(crate) fn get_todos() -> Vec<TodoItem> {
    TODO_STATE.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Replace the todo list with new items
pub(crate) fn set_todos(todos: Vec<TodoItem>) {
    if let Ok(mut guard) = TODO_STATE.lock() {
        *guard = todos;
    }
}

/// Clear the todo list
pub(crate) fn clear_todos() {
    if let Ok(mut guard) = TODO_STATE.lock() {
        guard.clear();
    }
}

/// Format the todo list for display
fn format_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "Todo list is empty.".to_string();
    }

    let mut lines = Vec::new();
    for item in todos {
        let (indicator, text) = match item.status {
            TodoStatus::Pending => ("[ ]", &item.content),
            TodoStatus::InProgress => ("[-]", &item.active_form),
            TodoStatus::Completed => ("[✓]", &item.content),
        };
        lines.push(format!("  {} {}", indicator, text));
    }
    lines.join("\n")
}

// === TodoWrite Tool ===

/// Tool for updating the todo list
pub(crate) struct TodoWrite;

#[derive(Debug, Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

impl Tool for TodoWrite {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_write".to_string(),
            description: "Update the todo list to show users your progress. Use this at the start of multi-step work (2+ steps or files). Update status to in_progress when starting each step and completed when done. Replaces the entire list with the provided items.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The updated todo list",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "What to do (imperative form, e.g. 'Run tests')"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Current status of the item"
                                },
                                "active_form": {
                                    "type": "string",
                                    "description": "Present continuous form (e.g. 'Running tests')"
                                }
                            },
                            "required": ["content", "status", "active_form"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let input: TodoWriteInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        // Update the global state
        set_todos(input.todos.clone());

        // Emit the update for display
        output::emit_todo_list(output, input.todos.clone());

        // Return formatted list as confirmation
        let formatted = format_todos(&input.todos);
        ToolResult::success(tool_use_id, format!("Todo list updated:\n{}", formatted))
    }
}

// === TodoRead Tool ===

/// Tool for reading the current todo list
pub(crate) struct TodoRead;

impl Tool for TodoRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_read".to_string(),
            description: "Read the current todo list.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        _input: serde_json::Value,
        _output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let todos = get_todos();
        let formatted = format_todos(&todos);
        ToolResult::success(tool_use_id, formatted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_todo_status_serialization() {
        assert_eq!(
            serde_json::to_string(&TodoStatus::Pending).unwrap(),
            r#""pending""#
        );
        assert_eq!(
            serde_json::to_string(&TodoStatus::InProgress).unwrap(),
            r#""in_progress""#
        );
        assert_eq!(
            serde_json::to_string(&TodoStatus::Completed).unwrap(),
            r#""completed""#
        );
    }

    #[test]
    fn test_todo_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<TodoStatus>(r#""pending""#).unwrap(),
            TodoStatus::Pending
        );
        assert_eq!(
            serde_json::from_str::<TodoStatus>(r#""in_progress""#).unwrap(),
            TodoStatus::InProgress
        );
        assert_eq!(
            serde_json::from_str::<TodoStatus>(r#""completed""#).unwrap(),
            TodoStatus::Completed
        );
    }

    #[test]
    fn test_todo_item_serialization() {
        let item = TodoItem {
            content: "Run tests".to_string(),
            status: TodoStatus::InProgress,
            active_form: "Running tests".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""content":"Run tests""#));
        assert!(json.contains(r#""status":"in_progress""#));
        assert!(json.contains(r#""active_form":"Running tests""#));
    }

    #[test]
    fn test_format_todos_empty() {
        assert_eq!(format_todos(&[]), "Todo list is empty.");
    }

    #[test]
    fn test_format_todos() {
        let todos = vec![
            TodoItem {
                content: "First task".to_string(),
                status: TodoStatus::Completed,
                active_form: "Doing first task".to_string(),
            },
            TodoItem {
                content: "Second task".to_string(),
                status: TodoStatus::InProgress,
                active_form: "Doing second task".to_string(),
            },
            TodoItem {
                content: "Third task".to_string(),
                status: TodoStatus::Pending,
                active_form: "Doing third task".to_string(),
            },
        ];
        let formatted = format_todos(&todos);
        assert!(formatted.contains("[✓] First task")); // completed shows content
        assert!(formatted.contains("[-] Doing second task")); // in_progress shows active_form
        assert!(formatted.contains("[ ] Third task")); // pending shows content
    }

    #[test]
    fn test_global_state() {
        // Clear first
        clear_todos();
        assert!(get_todos().is_empty());

        // Set some todos
        let todos = vec![TodoItem {
            content: "Test".to_string(),
            status: TodoStatus::Pending,
            active_form: "Testing".to_string(),
        }];
        set_todos(todos.clone());
        assert_eq!(get_todos(), todos);

        // Clear again
        clear_todos();
        assert!(get_todos().is_empty());
    }

    #[tokio::test]
    async fn test_todo_write_execute() {
        let tool = TodoWrite;
        let input = serde_json::json!({
            "todos": [
                {
                    "content": "Write code",
                    "status": "in_progress",
                    "active_form": "Writing code"
                }
            ]
        });

        let result = tool
            .execute(
                "test-id",
                input,
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Todo list updated"));
        assert!(result.content.contains("Writing code"));

        // Verify state was updated
        let todos = get_todos();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "Write code");
    }

    #[tokio::test]
    async fn test_todo_read_execute() {
        // Set up some state first
        set_todos(vec![TodoItem {
            content: "Read test".to_string(),
            status: TodoStatus::Pending,
            active_form: "Reading test".to_string(),
        }]);

        let tool = TodoRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({}),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("[ ] Read test"));
    }

    #[tokio::test]
    async fn test_todo_write_invalid_input() {
        let tool = TodoWrite;
        let input = serde_json::json!({
            "invalid": "data"
        });

        let result = tool
            .execute(
                "test-id",
                input,
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Invalid input"));
    }
}
