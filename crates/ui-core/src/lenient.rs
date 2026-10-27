//! Reading a saved view setting that a newer build wrote.
//!
//! The onboarding draft is one JSON document. A closed enum field in it (the triage table on
//! screen, the Queue's order preset) makes serde reject the WHOLE document when a newer build
//! saved a value this build does not know, and the reviewer's dispositions would look lost
//! because the draft failed to load. A view setting is not worth a review: [`deserialize`]
//! reads an unknown name, or a value of an unexpected shape, as the type's default instead.
//!
//! Used by `#[serde(default, deserialize_with = "camerata_ui_core::lenient::deserialize")]` on
//! every such field of the draft.

use serde::{Deserialize, Deserializer};

/// A unit enum written to the draft by name, read back leniently.
pub trait WireEnum: Default + Sized {
    /// The variant the wire name `name` stands for; `None` for a name this build does not know.
    fn from_wire(name: &str) -> Option<Self>;
}

/// `deserialize_with` for a [`WireEnum`] field: the named variant, else `T::default()`.
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: WireEnum,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Name(String),
        // A number, an object, a list, null: anything a future build might write instead.
        Other(serde::de::IgnoredAny),
    }
    Ok(match Wire::deserialize(deserializer)? {
        Wire::Name(name) => T::from_wire(&name).unwrap_or_default(),
        Wire::Other(_) => T::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq)]
    enum Mode {
        #[default]
        Plain,
        Fancy,
    }

    impl WireEnum for Mode {
        fn from_wire(name: &str) -> Option<Self> {
            match name {
                "Plain" => Some(Mode::Plain),
                "Fancy" => Some(Mode::Fancy),
                _ => None,
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct Doc {
        #[serde(default, deserialize_with = "deserialize")]
        mode: Mode,
        kept: u32,
    }

    fn read(json: &str) -> Doc {
        serde_json::from_str(json).expect("a lenient field never fails the document")
    }

    #[test]
    fn a_known_name_is_read_as_itself() {
        assert_eq!(read(r#"{"mode":"Fancy","kept":7}"#).mode, Mode::Fancy);
    }

    #[test]
    fn an_unknown_name_or_shape_is_the_default_and_the_rest_of_the_document_survives() {
        for json in [
            r#"{"mode":"Holographic","kept":7}"#,
            r#"{"mode":null,"kept":7}"#,
            r#"{"mode":42,"kept":7}"#,
            r#"{"mode":{"future":true},"kept":7}"#,
            r#"{"mode":["a"],"kept":7}"#,
            r#"{"kept":7}"#,
        ] {
            let doc = read(json);
            assert_eq!(doc.mode, Mode::Plain, "{json}");
            assert_eq!(doc.kept, 7, "{json}");
        }
    }
}
