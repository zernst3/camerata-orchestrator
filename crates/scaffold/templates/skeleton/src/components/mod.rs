//! The design system's primitives layer (RUST-DIOXUS-14): every common UI element
//! has exactly one canonical implementation here, and pages compose these rather
//! than reimplementing an element inline. Palette, spacing, radii, and focus rings
//! live in `assets/design/tokens.css` + `assets/design/components.css`; these
//! components only wire the markup + the CSS class names those files style.

mod app_shell;
mod button;
mod card;
mod field;

pub use app_shell::AppShell;
pub use button::{Button, ButtonVariant};
pub use card::Card;
pub use field::Field;
