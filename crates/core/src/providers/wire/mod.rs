//! The wire formats providers speak, shared by all of them. A request is
//! always built as an OpenAI chat completion (`chat.rs`) and turned into
//! another format only on its way out; what comes back is turned into the
//! chat chunks it stands for. So a provider never builds a request itself:
//! it names its format.

pub(crate) mod anthropic;
pub(crate) mod openai_responses;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI's chat completions, which most servers copy.
    OpenAiChat,
    /// Anthropic's Messages API.
    Anthropic,
    /// OpenAI's Responses API, which alone some backends speak.
    OpenAiResponses,
}
