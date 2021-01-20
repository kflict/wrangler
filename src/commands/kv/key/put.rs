// TODO: (gabbi) This file should use cloudflare-rs instead of our http::legacy_auth_client
// when https://github.com/cloudflare/cloudflare-rs/issues/26 is handled (this is
// because the SET key request body is not json--it is the raw value).

use std::fs;
use std::fs::metadata;

use cloudflare::framework::response::ApiFailure;
use url::Url;

use crate::commands::kv;
use crate::http;
use crate::settings::global_user::GlobalUser;
use crate::settings::toml::Target;
use crate::terminal::message::{Message, StdOut};
use reqwest::blocking::multipart;
use reqwest::blocking::Body;
use regex::Regex;

pub struct KVMetaData {
    pub namespace_id: String,
    pub key: String,
    pub value: String,
    pub is_file: bool,
    pub expiration: Option<String>,
    pub expiration_ttl: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

pub fn parse_metadata(arg: Option<&str>) -> Result<Option<serde_json::Value>, failure::Error> {
    match arg {
        None => Ok(None),
        Some(s) => {
            match serde_json::from_str(s) {
                Ok(v) => Ok(Some(v)),
                Err(e) => {
                    // try to help users that forget to double-quote a JSON string
                    let re = Regex::new(r#"^['"]?[^"'{}\[\]]*['"]?$"#)?;
                    if re.is_match(s) {
                        failure::bail!("did you remember to double quote strings, like --metadata '\"{}\"'", s)
                    }
                    failure::bail!(e.to_string())
                },
            }
        }
    }
}

pub fn put(target: &Target, user: &GlobalUser, data: KVMetaData) -> Result<(), failure::Error> {
    kv::validate_target(target)?;

    let api_endpoint = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/storage/kv/namespaces/{}/values/{}",
        target.account_id,
        &data.namespace_id,
        kv::url_encode_key(&data.key)
    );

    // Add expiration and expiration_ttl query options as necessary.
    let mut query_params: Vec<(&str, &str)> = vec![];

    if let Some(exp) = &data.expiration {
        query_params.push(("expiration", exp))
    };
    if let Some(ttl) = &data.expiration_ttl {
        query_params.push(("expiration_ttl", ttl))
    };
    let url = Url::parse_with_params(&api_endpoint, query_params)?;

    let res = get_response(data, user, &url)?;

    let response_status = res.status();
    if response_status.is_success() {
        StdOut::success("Success")
    } else {
        // This is logic pulled from cloudflare-rs for pretty error formatting right now;
        // it will be redundant when we switch to using cloudflare-rs for all API requests.
        let parsed = res.json();
        let errors = parsed.unwrap_or_default();
        print!(
            "{}",
            kv::format_error(ApiFailure::Error(response_status, errors))
        );
    }

    Ok(())
}

fn get_response(
    data: KVMetaData,
    user: &GlobalUser,
    url: &Url,
) -> Result<reqwest::blocking::Response, failure::Error> {
    let url_into_str = url.to_string();
    let client = http::legacy_auth_client(user);
    let res = match data.metadata {
        Some(metadata) => {
            let part = if data.is_file {
                multipart::Part::file(&data.value)?
            } else {
                multipart::Part::text(data.value)
            };
            let form = multipart::Form::new()
                .part("value", part)
                .text("metadata", metadata.to_string());
            client.put(&url_into_str).multipart(form).send()?
        }
        None => {
            let value_body = get_request_body(data)?;
            client.put(&url_into_str).body(value_body).send()?
        }
    };
    Ok(res)
}

// If is_file is true, overwrite value to be the contents of the given
// filename in the 'value' arg.
fn get_request_body(data: KVMetaData) -> Result<Body, failure::Error> {
    if data.is_file {
        match &metadata(&data.value) {
            Ok(file_type) if file_type.is_file() => {
                let file = fs::File::open(&data.value)?;
                Ok(file.into())
            }
            Ok(file_type) if file_type.is_dir() => failure::bail!(
                "--path argument takes a file, {} is a directory",
                data.value
            ),
            // last remaining value is symlink
            Ok(_) => failure::bail!("--path argument takes a file, {} is a symlink", data.value),
            Err(e) => failure::bail!("{}", e),
        }
    } else {
        Ok(data.value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_parser_legal() {
        for input in vec![
            "true",
            "false",
            "123.456",
            r#""some string""#,
            "[1, 2]",
            "{\"key\": \"value\"}",
        ] {
            assert!(parse_metadata(Some(input)).is_ok());
        }
    }

    #[test]
    fn metadata_parser_illegal() {
        for input in vec!["something", "{key: 123}", "[1, 2"] {
            assert!(parse_metadata(Some(input)).is_err());
        }
    }

    #[test]
    fn metadata_parser_error_message_unquoted_string_error_message() -> Result<(), &'static str> {
        for input in vec!["abc", "'abc'", "'abc", "abc'", "\"abc", "abc\""] {
            match parse_metadata(Some(input)) {
                Ok(_) => return Err("illegal value was parsed successfully"),
                Err(e) => {
                    let expected_message = format!(
                        "did you remember to double quote strings, like --metadata '\"{}\"'", input);
                    assert_eq!(expected_message, e.to_string());
                },
            }
        }
        Ok(())
    }
}
