//! Page-level components (one module per route). Pages compose primitives from
//! `crate::components`; primitives never compose pages (RUST-DIOXUS-14).

mod home;

pub use home::Home;
