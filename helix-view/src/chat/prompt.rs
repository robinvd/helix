use crate::Editor;

use super::{chat_state::Message, context::FILE_CONTEXT};

const HELP_MSG: &'static str = r#"When you need additional context, request it using this format:

> #<command>:`<input>`

Examples:
> #file:`path/to/file.js`        (loads specific file)
> #buffers:`visible`             (loads all visible buffers)
> #git:`staged`                  (loads git staged changes)
> #system:`uname -a`             (loads system information)

Guidelines:
- Always request context when needed rather than guessing about files or code
- Use the > format on a new line when requesting context
- Output context commands directly - never ask if the user wants to provide information
- Assume the user will provide requested context in their next response

Available context providers and their usage:"#;

/// Create a ai prompt from the history and context
///
/// TODO: add system prompt option
pub fn format_prompt(
    editor: &Editor,
    prompt: &str,
    history: &[Message],
    context: &[super::context::Context],
) -> Result<Vec<Message>, anyhow::Error> {
    let mut messages = Vec::new();
    let mut system_prompt = prompt.to_owned();

    let enabled_contexts = &[FILE_CONTEXT];
    if enabled_contexts.len() > 0 {
        let context_instructions = enabled_contexts
            .iter()
            .map(|provider| format!(" - #{}: {}", provider.name, provider.description))
            .collect::<Vec<_>>()
            .join("\n\n");
        system_prompt = format!("{system_prompt}\n\n{HELP_MSG}\n{context_instructions}");
    }
    messages.push(Message {
        content: system_prompt,
        role: "system".to_string(),
    });
    let context_msg = Message {
        content: context
            .iter()
            .map(|item| item.resolve(editor))
            .collect::<Result<Vec<_>, anyhow::Error>>()?
            .join("\n"),
        role: "system".to_owned(),
    };
    messages.push(context_msg);
    messages.extend(history.iter().cloned());
    Ok(messages)
}
