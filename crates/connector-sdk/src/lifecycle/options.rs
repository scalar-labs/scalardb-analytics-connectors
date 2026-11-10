//! Validated connection options and the secret values among them.

use std::{collections::BTreeMap, fmt};

use zeroize::Zeroize;

use super::{ConnectorDescriptor, OptionValueType};
use crate::{ConnectorError, ErrorKind};

/// A credential value that never prints itself and is erased when dropped.
pub struct SecretString(String);

impl SecretString {
    /// Takes ownership of `value` so that it is erased with this value.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Takes ownership of received bytes; bytes that are not UTF-8 are erased
    /// and `None` is returned.
    pub fn from_utf8(bytes: Vec<u8>) -> Option<Self> {
        match String::from_utf8(bytes) {
            Ok(value) => Some(Self(value)),
            Err(error) => {
                error.into_bytes().zeroize();
                None
            }
        }
    }

    /// Returns the value for passing to Data Source access.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString(<redacted>)")
    }
}

/// One validated option value.
#[derive(Debug)]
pub enum OptionValue {
    Plain(String),
    /// The value of an option the descriptor marks as secret.
    Secret(SecretString),
}

/// Connection options validated against a [`ConnectorDescriptor`].
///
/// Every key is declared by the descriptor, every required key is present,
/// and every value has its declared form. Omitted options stay absent rather
/// than being filled with defaults; the connector owns their meaning.
#[derive(Debug)]
pub struct ConnectionOptions {
    values: BTreeMap<String, OptionValue>,
}

impl ConnectionOptions {
    /// Validates caller options against `descriptor`.
    ///
    /// Values arrive as [`SecretString`]s, as received, so a rejected value
    /// and the values after it are erased whatever their keys. Errors are
    /// `invalid settings` naming the option and never contain a value.
    pub fn parse(
        descriptor: &ConnectorDescriptor,
        options: impl IntoIterator<Item = (String, SecretString)>,
    ) -> Result<Self, ConnectorError> {
        let mut values = BTreeMap::new();
        for (key, value) in options {
            let Some(declared) = descriptor.options.iter().find(|option| option.key == key) else {
                return Err(invalid_settings(format!("option `{key}` is not accepted")));
            };
            if values.contains_key(&key) {
                return Err(invalid_settings(format!("option `{key}` is given twice")));
            }
            if !has_form(value.expose(), declared.value_type) {
                return Err(invalid_settings(format!(
                    "option `{key}` must be {}",
                    form_name(declared.value_type)
                )));
            }
            let value = if declared.secret {
                OptionValue::Secret(value)
            } else {
                OptionValue::Plain(value.expose().to_owned())
            };
            values.insert(key, value);
        }
        if let Some(missing) = descriptor
            .options
            .iter()
            .find(|option| option.required && !values.contains_key(&option.key))
        {
            return Err(invalid_settings(format!(
                "option `{}` is required",
                missing.key
            )));
        }
        Ok(Self { values })
    }

    /// Returns the value given for `key`, if any.
    pub fn get(&self, key: &str) -> Option<&OptionValue> {
        self.values.get(key)
    }
}

fn has_form(value: &str, value_type: OptionValueType) -> bool {
    match value_type {
        OptionValueType::String => true,
        OptionValueType::Integer => value.parse::<i64>().is_ok(),
        OptionValueType::Boolean => matches!(value, "true" | "false"),
    }
}

fn form_name(value_type: OptionValueType) -> &'static str {
    match value_type {
        OptionValueType::String => "text",
        OptionValueType::Integer => "a decimal integer",
        OptionValueType::Boolean => "`true` or `false`",
    }
}

fn invalid_settings(message: String) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidSettings, false, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataSourceAccess, OptionDescriptor};

    fn descriptor() -> ConnectorDescriptor {
        let option = |key: &str, secret, required, value_type| OptionDescriptor {
            key: key.to_owned(),
            secret,
            required,
            value_type,
            default: None,
            description: String::new(),
        };
        ConnectorDescriptor {
            name: "example".to_owned(),
            version: "1.0.0".to_owned(),
            connectors: vec!["example".to_owned()],
            options: vec![
                option("host", false, true, OptionValueType::String),
                option("port", false, false, OptionValueType::Integer),
                option("password", true, false, OptionValueType::String),
            ],
            access: DataSourceAccess::Direct,
        }
    }

    fn parse(pairs: &[(&str, &str)]) -> Result<ConnectionOptions, ConnectorError> {
        ConnectionOptions::parse(
            &descriptor(),
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), SecretString::new((*value).to_owned()))),
        )
    }

    #[test]
    fn declared_options_are_accepted_and_secrets_are_held_as_secrets() {
        let options = parse(&[("host", "db"), ("port", "5432"), ("password", "hunter2")]).unwrap();
        assert!(matches!(options.get("host"), Some(OptionValue::Plain(host)) if host == "db"));
        assert!(matches!(
            options.get("password"),
            Some(OptionValue::Secret(secret)) if secret.expose() == "hunter2"
        ));
        assert!(!format!("{options:?}").contains("hunter2"));
        assert!(options.get("port").is_some());
    }

    #[test]
    fn invalid_options_name_the_option_without_its_value() {
        for (pairs, key) in [
            (&[("host", "db"), ("user", "alice")][..], "user"),
            (&[("host", "db"), ("host", "other")][..], "host"),
            (&[("host", "db"), ("port", "secret-port")][..], "port"),
            (&[("password", "hunter2")][..], "host"),
        ] {
            let error = parse(pairs).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidSettings);
            assert!(!error.retryable());
            assert!(error.safe_message().contains(&format!("`{key}`")));
            for (_, value) in pairs {
                assert!(
                    !error.safe_message().contains(value),
                    "{}",
                    error.safe_message()
                );
            }
        }
    }
}
