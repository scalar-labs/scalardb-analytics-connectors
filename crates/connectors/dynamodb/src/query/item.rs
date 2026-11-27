//! The value of the `item` column: an item as DynamoDB JSON.
//!
//! Each attribute value is an object with one member, named by its DynamoDB
//! type descriptor, as DynamoDB's own API writes it: a number keeps its
//! exact decimal text under `N`, a set stays a set under `SS`, `NS`, or `BS`,
//! and binary is base64 under `B` or in `BS`. The members of every map,
//! including the item itself, are written in the byte order of their names,
//! and the elements of every set in the byte order of their text or bytes,
//! so the text of an item depends only on its attributes: DynamoDB keeps no
//! order of map members or set elements. List elements keep their order.

use std::collections::HashMap;

use aws_sdk_dynamodb::{primitives::Blob, types::AttributeValue};
use base64::{Engine, engine::general_purpose::STANDARD};

/// An attribute value of a type the SDK does not know, which DynamoDB JSON
/// cannot be written for.
#[derive(Debug)]
pub(crate) struct UnknownType;

/// Writes `item` as DynamoDB JSON.
pub(crate) fn item_json(item: &HashMap<String, AttributeValue>) -> Result<String, UnknownType> {
    let mut json = String::new();
    write_map(item, &mut json)?;
    Ok(json)
}

fn write_map(map: &HashMap<String, AttributeValue>, json: &mut String) -> Result<(), UnknownType> {
    let mut names: Vec<&String> = map.keys().collect();
    names.sort_unstable();
    json.push('{');
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        write_string(name, json);
        json.push(':');
        write_value(&map[name], json)?;
    }
    json.push('}');
    Ok(())
}

fn write_value(value: &AttributeValue, json: &mut String) -> Result<(), UnknownType> {
    let mut member = |descriptor: &str| {
        json.push_str("{\"");
        json.push_str(descriptor);
        json.push_str("\":");
    };
    match value {
        AttributeValue::S(text) => {
            member("S");
            write_string(text, json);
        }
        AttributeValue::N(number) => {
            member("N");
            write_string(number, json);
        }
        AttributeValue::B(bytes) => {
            member("B");
            write_binary(bytes, json);
        }
        AttributeValue::Bool(flag) => {
            member("BOOL");
            json.push_str(if *flag { "true" } else { "false" });
        }
        AttributeValue::Null(flag) => {
            member("NULL");
            json.push_str(if *flag { "true" } else { "false" });
        }
        AttributeValue::Ss(texts) => {
            member("SS");
            write_list(sorted(texts, String::as_bytes), json, |text, json| {
                write_string(text, json);
                Ok(())
            })?;
        }
        AttributeValue::Ns(numbers) => {
            member("NS");
            write_list(sorted(numbers, String::as_bytes), json, |number, json| {
                write_string(number, json);
                Ok(())
            })?;
        }
        AttributeValue::Bs(binaries) => {
            member("BS");
            write_list(sorted(binaries, Blob::as_ref), json, |bytes, json| {
                write_binary(bytes, json);
                Ok(())
            })?;
        }
        AttributeValue::L(values) => {
            member("L");
            write_list(values, json, write_value)?;
        }
        AttributeValue::M(map) => {
            member("M");
            write_map(map, json)?;
        }
        _ => return Err(UnknownType),
    }
    json.push('}');
    Ok(())
}

fn write_list<'a, T: 'a>(
    elements: impl IntoIterator<Item = &'a T>,
    json: &mut String,
    write: impl Fn(&T, &mut String) -> Result<(), UnknownType>,
) -> Result<(), UnknownType> {
    json.push('[');
    for (index, element) in elements.into_iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        write(element, json)?;
    }
    json.push(']');
    Ok(())
}

/// The elements of a set in the unsigned byte order of `bytes`.
fn sorted<T>(elements: &[T], bytes: impl Fn(&T) -> &[u8]) -> Vec<&T> {
    let mut sorted: Vec<&T> = elements.iter().collect();
    sorted.sort_unstable_by(|left, right| bytes(left).cmp(bytes(right)));
    sorted
}

fn write_string(text: &str, json: &mut String) {
    json.push_str(&serde_json::to_string(text).expect("a string serializes to JSON"));
}

fn write_binary(bytes: &Blob, json: &mut String) {
    json.push('"');
    json.push_str(&STANDARD.encode(bytes.as_ref()));
    json.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(attributes: Vec<(&str, AttributeValue)>) -> HashMap<String, AttributeValue> {
        attributes
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect()
    }

    #[test]
    fn every_type_is_written_under_its_descriptor_with_names_and_set_elements_in_byte_order() {
        let nested = item(vec![
            ("z", AttributeValue::Null(true)),
            ("a", AttributeValue::Bool(false)),
        ]);
        let json = item_json(&item(vec![
            (
                "s",
                AttributeValue::S("\u{e9}\"\n\u{1}\u{1f600}".to_owned()),
            ),
            ("n", AttributeValue::N("-1.50E+3".to_owned())),
            ("b", AttributeValue::B(Blob::new([0, 255]))),
            (
                "ss",
                AttributeValue::Ss(vec!["b".to_owned(), "a".to_owned()]),
            ),
            (
                "ns",
                AttributeValue::Ns(vec!["10".to_owned(), "9".to_owned()]),
            ),
            (
                "bs",
                AttributeValue::Bs(vec![Blob::new([0x80]), Blob::new([1]), Blob::new([])]),
            ),
            (
                "l",
                AttributeValue::L(vec![
                    AttributeValue::S("x".to_owned()),
                    AttributeValue::M(HashMap::new()),
                    AttributeValue::L(Vec::new()),
                ]),
            ),
            ("m", AttributeValue::M(nested)),
            ("B", AttributeValue::Bool(true)),
            ("\u{e9}", AttributeValue::N("0".to_owned())),
        ]))
        .unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"B":{"BOOL":true},"b":{"B":"AP8="},"bs":{"BS":["","AQ==","gA=="]},"#,
                r#""l":{"L":[{"S":"x"},{"M":{}},{"L":[]}]},"#,
                r#""m":{"M":{"a":{"BOOL":false},"z":{"NULL":true}}},"#,
                r#""n":{"N":"-1.50E+3"},"ns":{"NS":["10","9"]},"#,
                r#""s":{"S":"é\"\n\u0001😀"},"ss":{"SS":["a","b"]},"é":{"N":"0"}}"#,
            )
        );
        // The text is JSON.
        serde_json::from_str::<serde_json::Value>(&json).unwrap();
    }

    #[test]
    fn an_empty_item_is_an_empty_object() {
        assert_eq!(item_json(&HashMap::new()).unwrap(), "{}");
    }
}
