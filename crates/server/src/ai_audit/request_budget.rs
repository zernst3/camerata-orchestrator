//! How large a model request may be (Fable engine review item M1): the context window of a model
//! and the conservative size estimate a prompt is checked against before it is sent.

/// The window assumed for a model the registry does not know: the smallest Claude window, so an
/// unknown model is never sent more than the smallest one takes.
pub(super) const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 200_000;

/// The context window of `model` in tokens, or [`DEFAULT_CONTEXT_WINDOW_TOKENS`] for a model the
/// registry does not list.
pub(super) fn context_window_tokens(model: &str) -> usize {
    crate::model_registry::context_window_for(model).map_or(DEFAULT_CONTEXT_WINDOW_TOKENS, |w| {
        usize::try_from(w).unwrap_or(usize::MAX)
    })
}
