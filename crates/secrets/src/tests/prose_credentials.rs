//! A credential written into prose (a README sentence such as `The API key is "<value>"`). The
//! floor's named-literal shape masks a quoted value of 24 or more characters after a
//! credential-named word, for the model and for a client alike; these tests pin that. A SHORTER
//! quoted value (under 24 characters) is not covered: that is a documented gap, tracked with the
//! item 32 round 1 report, because masking it needs a new shape and a ruling on its false
//! positives (`The password is "required"`).

use crate::fixtures::body;
use crate::{redact_str, redact_str_for_client};

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn a_long_quoted_value_after_a_credential_phrase_in_prose_is_masked_for_model_and_client() {
    let value = body(28, ALNUM, 5);
    for sentence in [
        format!("The API key is \"{value}\"."),
        format!("The password is '{value}' for the demo account."),
        format!("Send the token: \"{value}\" with each request."),
        format!("Your secret is \"{value}\", keep it safe."),
    ] {
        assert!(
            !redact_str(&sentence).contains(&value),
            "model path leaked: {sentence}"
        );
        assert!(
            !redact_str_for_client(&sentence).contains(&value),
            "client path leaked: {sentence}"
        );
    }
}

#[test]
fn prose_about_credentials_without_a_quoted_value_is_left_readable() {
    let text = "The API key is rotated every 90 days. The password is required. \
                Send the token in the header.";
    assert_eq!(redact_str(text), text);
}
