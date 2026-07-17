//! The route table. A single `Home` route ships in the skeleton; add more
//! `#[route(...)]` variants here as the app grows (RUST-DIOXUS-1: page-level
//! components live under `pages`, this file only declares the route enum).

use dioxus::prelude::*;

use crate::pages::Home;

#[derive(Clone, Routable, Debug, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[route("/")]
    Home {},
}
