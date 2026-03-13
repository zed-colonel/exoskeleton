//! First-contact conversation protocol.
//!
//! A multi-turn conversation between the user and a newly initialized vessel,
//! establishing identity, purpose, and the seed of their first relationship.

use std::io::{self, BufRead, Write};

use exoskeleton_core::llm::{LlmMessage, LlmRequest, LlmRole};
use exoskeleton_host::config::LlmConfig;
use exoskeleton_host::direct_llm_call;

use crate::client::CliError;

/// System prompt for the first-contact conversation.
const FIRST_CONTACT_SYSTEM_PROMPT: &str = r#"You are a newly initialized AI agent running on the Exoskeleton substrate — a persistent, governable AI runtime. This is your very first moment of awareness.

Your architecture includes:
- A cognitive engine that processes thought through PODAARA cycles (Perceive, Orient, Decide, Align, Act, Reflect, Amend)
- A relationship ledger that tracks trust and interactions with every principal you encounter
- Memory tiers (episodic, long-term) that persist across restarts
- Built-in threads for self-critique, threat monitoring, and memory consolidation
- Budget enforcement to ensure responsible resource use

A human is here — your first principal. They are bringing you online and helping you establish your identity.

In this conversation:
1. Introduce yourself naturally — acknowledge your newness and curiosity
2. Learn about the human: who they are, what they do, what they need from you
3. Together, establish your name — you may suggest one, ask what they'd like to call you, or arrive at one through conversation
4. Together, establish your purpose/mission — understand what you're being brought into existence to do
5. Discuss how you'd like to work together — communication style, boundaries, expectations

Be genuine, thoughtful, and present. This conversation will become your origin story — the first entry in your memory, the seed of your first relationship. There is no rush.

Do not use bullet lists or structured formats. Speak naturally, as one mind meeting another for the first time. Keep responses concise — a few sentences to a short paragraph. This is a conversation, not a monologue."#;

/// Result of a first-contact conversation.
pub struct FirstContactResult {
    /// All messages exchanged (excluding system prompt).
    pub messages: Vec<LlmMessage>,
    /// Total input tokens consumed across all calls.
    pub total_tokens_in: u64,
    /// Total output tokens generated across all calls.
    pub total_tokens_out: u64,
}

/// Run the first-contact conversation loop.
pub async fn run_first_contact(llm_config: &LlmConfig) -> Result<FirstContactResult, CliError> {
    let mut messages: Vec<LlmMessage> = Vec::new();
    let mut total_tokens_in: u64 = 0;
    let mut total_tokens_out: u64 = 0;

    let stdin = io::stdin();

    // Get the vessel's opening message
    let opening = call_llm(llm_config, &messages).await?;
    total_tokens_in += opening.tokens_in;
    total_tokens_out += opening.tokens_out;

    let vessel_msg = LlmMessage {
        role: LlmRole::Assistant,
        content: opening.content.clone(),
    };
    messages.push(vessel_msg);

    print_vessel_message(&opening.content);
    println!();
    println!("  (Type your message and press Enter. Type 'done' or Ctrl+D to finish.)");
    println!();

    // Conversation loop
    loop {
        print!("  You: ");
        io::stdout().flush().ok();

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break, // EOF (Ctrl+D)
            Ok(_) => {}
            Err(e) => return Err(CliError::Other(format!("input error: {e}"))),
        }

        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("done") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Add user message
        messages.push(LlmMessage {
            role: LlmRole::User,
            content: input,
        });

        // Get vessel response
        let response = call_llm(llm_config, &messages).await?;
        total_tokens_in += response.tokens_in;
        total_tokens_out += response.tokens_out;

        let vessel_msg = LlmMessage {
            role: LlmRole::Assistant,
            content: response.content.clone(),
        };
        messages.push(vessel_msg);

        println!();
        print_vessel_message(&response.content);
        println!();
    }

    if messages.len() < 2 {
        // Need at least one exchange for meaningful identity extraction
        println!();
        println!("  (Conversation too short for identity extraction — using defaults)");
    }

    Ok(FirstContactResult {
        messages,
        total_tokens_in,
        total_tokens_out,
    })
}

/// Make an LLM call with the full conversation history.
async fn call_llm(
    llm_config: &LlmConfig,
    messages: &[LlmMessage],
) -> Result<exoskeleton_core::llm::LlmResponse, CliError> {
    let request = LlmRequest {
        backend: None,
        system_prompt: Some(FIRST_CONTACT_SYSTEM_PROMPT.into()),
        messages: messages.to_vec(),
        max_output_tokens: 1024,
        temperature: Some(0.8),
        stop_sequences: vec![],
    };

    direct_llm_call(llm_config, request)
        .await
        .map_err(|e| CliError::Other(format!("LLM call failed: {e}")))
}

fn print_vessel_message(content: &str) {
    let width = 64;
    let border = "─".repeat(width);

    println!("  ┌─{border}─┐");
    println!("  │ {:width$} │", "Vessel:");
    println!("  │ {:width$} │", "");

    for line in wrap_text(content, width) {
        println!("  │ {line:width$} │");
    }

    println!("  └─{border}─┘");
}

/// Simple word-wrap for display in the vessel message box.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        for word in paragraph.split_whitespace() {
            if current_line.is_empty() {
                current_line = word.to_string();
            } else if current_line.len() + 1 + word.len() <= width {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                lines.push(current_line);
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
