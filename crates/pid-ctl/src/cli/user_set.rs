/// Distinguishes a value the user explicitly provided on the CLI from one
/// that was computed as a default.
///
/// `set_if_default` is the only mutating method: it silently ignores updates
/// when the variant is already `Explicit`, implementing the "don't override
/// user-provided values at runtime" policy used by `apply_runtime_interval`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UserSet<T> {
    Explicit(T),
    Default(T),
}

impl<T> UserSet<T> {
    pub(crate) fn value(&self) -> &T {
        match self {
            Self::Explicit(v) | Self::Default(v) => v,
        }
    }

    /// Updates the stored value only when this slot holds a default.
    /// No-op when the variant is `Explicit`.
    pub(crate) fn set_if_default(&mut self, v: T) {
        if let Self::Default(slot) = self {
            *slot = v;
        }
    }
}
