//! Reading a saved view setting that a newer build wrote.
//!
//! The onboarding draft is one JSON document. A closed enum field in it (the triage table on
//! screen, the Queue's order preset) makes serde reject the WHOLE document when a newer build
//! saved a value this build does not know, and the reviewer's dispositions would look lost
//! because the draft failed to load. A view setting is not worth a review: [`deserialize`]
//! reads an unknown name, or a value of an unexpected shape, as the type's default instead.
//!
//! Used by `#[serde(default, deserialize_with = "camerata_ui_core::lenient::deserialize")]` on
//! every such field of the draft. [`deserialize_or_default`] is the same idea for a saved STRUCT
//! (the review timer's log): a shape this build cannot read is the default, not a failed draft.

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

/// `deserialize_with` for a STRUCT field that is saved data and not a view setting (the review
/// log): the value when it reads as `T`, else `T::default()`. The sibling of [`deserialize`] for
/// shapes that are not a [`WireEnum`]: a newer build may have changed the shape, and the draft's
/// scan, dispositions and view settings must still load.
pub fn deserialize_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire<T> {
        Value(T),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Wire::<T>::deserialize(deserializer)? {
        Wire::Value(value) => value,
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

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Saved {
        #[serde(default)]
        n: u32,
    }

    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "deserialize_or_default")]
        saved: Saved,
        kept: u32,
    }

    #[test]
    fn a_struct_that_reads_is_kept_and_one_that_does_not_is_the_default() {
        let read = |json: &str| -> Holder { serde_json::from_str(json).expect("never fails") };
        assert_eq!(read(r#"{"saved":{"n":4},"kept":7}"#).saved, Saved { n: 4 });
        for json in [
            r#"{"saved":"future","kept":7}"#,
            r#"{"saved":[1,2],"kept":7}"#,
            r#"{"saved":{"n":"not a number"},"kept":7}"#,
            r#"{"saved":null,"kept":7}"#,
            r#"{"kept":7}"#,
        ] {
            let holder = read(json);
            assert_eq!(holder.saved, Saved::default(), "{json}");
            assert_eq!(holder.kept, 7, "{json}");
        }
    }
}
