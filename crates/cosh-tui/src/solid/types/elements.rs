use crate::core::types::RenderContext;

/// Properties that should not be included in the style prop.
/// Matches the TS `NonStyledProps` union type.
///
/// In Rust, this is represented as a marker trait since there's no
/// equivalent of TypeScript's template literal types (`on${string}`).
pub trait NonStyledProps {}

/// Solid-specific props for all components.
/// In Rust, this is a minimal struct since there's no JSX event system.
pub struct ElementProps<TRenderable> {
    pub _phantom: std::marker::PhantomData<TRenderable>,
}

/// Base trait for any renderable constructor.
/// Analogous to TS `RenderableConstructor<TRenderable>`.
///
/// # Type parameters
/// - `TOptions`: The options type accepted by the constructor
/// - `TRenderable`: The renderable type produced
pub trait RenderableConstructor<TOptions, TRenderable> {
    /// Create a new instance with the given context and options.
    fn create<TCtx: RenderContext<TRenderable>>(ctx: &mut TCtx, options: TOptions) -> TRenderable;
}

/// Marker trait for widget option types.
/// Analogous to TS conditional types that determine which properties
/// should be excluded from styling for different renderable types.
///
/// Note: The TS original uses conditional types with real logic (different
/// exclusions per widget type). Rust lacks conditional types, so this is
/// a simplified marker trait.
pub trait GetNonStyledProperties {}

// Widget prop type aliases (placeholders — full definitions added as each
// widget (items 18–32) is ported).

// pub type TextProps = ();
// pub type SpanProps = ();
// pub type LinkProps = ();
// pub type BoxProps = ();
// pub type InputProps = ();
// pub type TextareaProps = ();
// pub type SelectProps = ();
// pub type AsciiFontProps = ();
// pub type TabSelectProps = ();
// pub type ScrollBoxProps = ();
// pub type CodeProps = ();
// pub type MarkdownProps = ();
