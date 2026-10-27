//! Reading a saved view setting that a newer build wrote.
//!
//! The onboarding draft is one JSON document. A closed enum field in it (the triage table on
//! screen, the Queue's order preset) makes serde reject the WHOLE document when a newer build
//! saved a value this build does not know, and the reviewer's dispositions would look lost
//! because the draft failed to load. A view setting is not worth a review: [`deserialize`]
//! reads an unknown name, or a value of an unexpected shape, as the type's default instead.
//!
//! Used by `#[serde(default, deserialize_with = "camerata_ui_core::lenient::deserialize")]` on
//! every such field of the draft. [`deserialize_or_default`] is the one fallback rule underneath
//! (and the same idea for a saved struct); [`read_map`] is the entry-by-entry read for saved
//! WORK (the review timer's log), where dropping the lot for one bad entry would lose hours.

use serde::{Deserialize, Deserializer};

/// A unit enum written to the draft by name, read back leniently.
pub trait WireEnum: Default + Sized {
    /// The variant the wire name `name` stands for; `None` for a name this build does not know.
    fn from_wire(name: &str) -> Option<Self>;
}

/// A [`WireEnum`] read by name; anything that is not a known name is the default.
struct Named<T>(T);

impl<T: WireEnum> Default for Named<T> {
    fn default() -> Self {
        Named(T::default())
    }
}

impl<'de, T: WireEnum> Deserialize<'de> for Named<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Named(T::from_wire(&name).unwrap_or_default()))
    }
}

/// `deserialize_with` for a [`WireEnum`] field: the named variant, else `T::default()`. A number,
/// an object, a list, null: anything a future build might write instead is the default too.
/// Built on [`deserialize_or_default`], so there is one fallback rule.
pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: WireEnum,
{
    deserialize_or_default::<D, Named<T>>(deserializer).map(|named| named.0)
}

/// `deserialize_with` for a field that is saved data and not a view setting: the value when it
/// reads as `T`, else `T::default()`. The one fallback rule; [`deserialize`] is this for a
/// [`WireEnum`]. A newer build may have changed the shape, and the draft's scan, dispositions and
/// view settings must still load.
///
/// Whole-value fallback is right for a view setting and WRONG for saved work: it would drop all of
/// it for one bad entry. The review log is read with [`read_map`] instead, entry by entry.
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

/// A JSON object read entry by entry: every entry that reads as `T` is kept, every one that does
/// not is skipped, and the second value says whether anything was skipped (or the value was not
/// an object at all). `None` is an absent map: empty, nothing skipped.
pub fn read_map<T: serde::de::DeserializeOwned>(
    value: Option<&serde_json::Value>,
) -> (std::collections::BTreeMap<String, T>, bool) {
    let mut out = std::collections::BTreeMap::new();
    let Some(value) = value else {
        return (out, false);
    };
    let Some(object) = value.as_object() else {
        return (out, true);
    };
    let mut skipped = false;
    for (key, entry) in object {
        match serde_json::from_value::<T>(entry.clone()) {
            Ok(read) => {
                out.insert(key.clone(), read);
            }
            Err(_) => skipped = true,
        }
    }
    (out, skipped)
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

    #[test]
    fn a_map_is_read_entry_by_entry_and_says_when_it_skipped() {
        let value = serde_json::json!({ "a": { "n": 1 }, "b": "bad", "c": { "n": 3 } });
        let (map, skipped) = read_map::<Saved>(Some(&value));
        assert!(skipped);
        assert_eq!(map.len(), 2);
        assert_eq!(map["a"], Saved { n: 1 });
        assert_eq!(map["c"], Saved { n: 3 });
        let clean = serde_json::json!({ "a": { "n": 1 } });
        assert!(!read_map::<Saved>(Some(&clean)).1);
        assert_eq!(read_map::<Saved>(None), (Default::default(), false));
        let not_a_map = serde_json::json!([1, 2]);
        assert_eq!(
            read_map::<Saved>(Some(&not_a_map)),
            (Default::default(), true)
        );
    }
}
