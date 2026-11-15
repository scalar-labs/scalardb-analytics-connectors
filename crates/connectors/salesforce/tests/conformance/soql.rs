//! The part of SOQL the mock org answers: `SELECT fields FROM object` with an
//! optional `WHERE` of comparisons joined by `AND` and `OR`.
//!
//! Conditions are evaluated as the fixture models Salesforce: text compares
//! without regard to case, `!=` is true for null, and every other comparison
//! with null is false. The opt-in tests against a real org check this model.

use std::{cmp::Ordering, collections::HashMap};

use super::mock::Record;

pub struct Query {
    pub fields: Vec<String>,
    pub object: String,
    pub condition: Option<Condition>,
}

pub enum Condition {
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
    Compare {
        field: String,
        operator: String,
        value: Literal,
    },
}

pub enum Literal {
    Null,
    /// A number, Boolean, date, or date-time, as written.
    Bare(String),
    Text(String),
}

/// The fields a condition reads.
pub fn fields(condition: &Condition) -> Vec<&String> {
    match condition {
        Condition::And(left, right) | Condition::Or(left, right) => {
            let mut fields = self::fields(left);
            fields.extend(self::fields(right));
            fields
        }
        Condition::Compare { field, .. } => vec![field],
    }
}

/// Parses a query, or `None` for text outside the supported part of SOQL.
pub fn parse(text: &str) -> Option<Query> {
    let tokens = tokenize(text)?;
    let mut parser = Parser { tokens, next: 0 };
    parser.keyword("SELECT")?;
    let mut fields = vec![parser.identifier()?];
    while parser.peek() == Some(",") {
        parser.next += 1;
        fields.push(parser.identifier()?);
    }
    parser.keyword("FROM")?;
    let object = parser.identifier()?;
    let condition = match parser.peek() {
        None => None,
        Some(_) => {
            parser.keyword("WHERE")?;
            Some(parser.disjunction()?)
        }
    };
    (parser.next == parser.tokens.len()).then_some(Query {
        fields,
        object,
        condition,
    })
}

fn tokenize(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&character) = chars.peek() {
        if character.is_whitespace() {
            chars.next();
        } else if matches!(character, '(' | ')' | ',') {
            tokens.push(character.to_string());
            chars.next();
        } else if matches!(character, '=' | '!' | '<' | '>') {
            let mut operator = chars.next()?.to_string();
            if chars.peek() == Some(&'=') {
                operator.push(chars.next()?);
            }
            tokens.push(operator);
        } else if character == '\'' {
            chars.next();
            let mut quoted = String::from("'");
            loop {
                match chars.next()? {
                    '\\' => quoted.push(chars.next()?),
                    '\'' => break,
                    other => quoted.push(other),
                }
            }
            tokens.push(quoted);
        } else {
            let mut word = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() || matches!(next, '(' | ')' | ',' | '=' | '!' | '<' | '>') {
                    break;
                }
                word.push(next);
                chars.next();
            }
            tokens.push(word);
        }
    }
    Some(tokens)
}

struct Parser {
    tokens: Vec<String>,
    next: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.next).map(String::as_str)
    }

    fn take(&mut self) -> Option<String> {
        let token = self.tokens.get(self.next)?.clone();
        self.next += 1;
        Some(token)
    }

    fn keyword(&mut self, keyword: &str) -> Option<()> {
        self.take()?.eq_ignore_ascii_case(keyword).then_some(())
    }

    fn identifier(&mut self) -> Option<String> {
        let token = self.take()?;
        token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
            .then_some(token)
    }

    fn disjunction(&mut self) -> Option<Condition> {
        let mut condition = self.conjunction()?;
        while self
            .peek()
            .is_some_and(|token| token.eq_ignore_ascii_case("OR"))
        {
            self.next += 1;
            condition = Condition::Or(Box::new(condition), Box::new(self.conjunction()?));
        }
        Some(condition)
    }

    fn conjunction(&mut self) -> Option<Condition> {
        let mut condition = self.atom()?;
        while self
            .peek()
            .is_some_and(|token| token.eq_ignore_ascii_case("AND"))
        {
            self.next += 1;
            condition = Condition::And(Box::new(condition), Box::new(self.atom()?));
        }
        Some(condition)
    }

    fn atom(&mut self) -> Option<Condition> {
        if self.peek() == Some("(") {
            self.next += 1;
            let condition = self.disjunction()?;
            (self.take()? == ")").then_some(condition)
        } else {
            let field = self.identifier()?;
            let operator = self.take()?;
            if !matches!(operator.as_str(), "=" | "!=" | "<" | "<=" | ">" | ">=") {
                return None;
            }
            let value = self.take()?;
            let value = if value.eq_ignore_ascii_case("null") {
                Literal::Null
            } else if let Some(text) = value.strip_prefix('\'') {
                Literal::Text(text.to_owned())
            } else {
                Literal::Bare(value)
            };
            Some(Condition::Compare {
                field,
                operator,
                value,
            })
        }
    }
}

/// Whether `record` satisfies `condition`; `types` maps lowercase field names
/// to their describe types.
pub fn holds(condition: &Condition, record: &Record, types: &HashMap<String, &str>) -> bool {
    match condition {
        Condition::And(left, right) => holds(left, record, types) && holds(right, record, types),
        Condition::Or(left, right) => holds(left, record, types) || holds(right, record, types),
        Condition::Compare {
            field,
            operator,
            value,
        } => {
            let raw = record
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(field))
                .map_or("null", |(_, raw)| raw.as_str());
            let is_null = raw == "null";
            let literal = match value {
                Literal::Null => {
                    return match operator.as_str() {
                        "=" => is_null,
                        "!=" => !is_null,
                        _ => false,
                    };
                }
                Literal::Bare(text) | Literal::Text(text) => text,
            };
            if is_null {
                return operator == "!=";
            }
            let field_type = types
                .get(&field.to_ascii_lowercase())
                .copied()
                .unwrap_or("");
            let ordering = compare(field_type, raw, literal);
            match operator.as_str() {
                "=" => ordering == Ordering::Equal,
                "!=" => ordering != Ordering::Equal,
                "<" => ordering == Ordering::Less,
                "<=" => ordering != Ordering::Greater,
                ">" => ordering == Ordering::Greater,
                _ => ordering != Ordering::Less,
            }
        }
    }
}

/// Compares a record's value, as JSON text, with a literal, by the field's
/// type.
fn compare(field_type: &str, raw: &str, literal: &str) -> Ordering {
    match field_type {
        "int" | "long" | "double" | "percent" | "currency" => scaled(raw).cmp(&scaled(literal)),
        "datetime" => instant(&unquote(raw)).cmp(&instant(literal)),
        "date" | "boolean" => unquote(raw).as_str().cmp(literal),
        _ => unquote(raw).to_lowercase().cmp(&literal.to_lowercase()),
    }
}

fn unquote(raw: &str) -> String {
    serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_owned())
}

/// A decimal number as an integer count of 10^-18.
fn scaled(text: &str) -> i128 {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let fraction = format!("{fraction:0<18}");
    let value: i128 = format!("{whole}{}", &fraction[..18])
        .parse()
        .expect("a number");
    if negative { -value } else { value }
}

/// A date-time written `YYYY-MM-DDTHH:MM:SS` with an optional fraction and
/// `Z` or `+0000`, as its UTC wall-clock text and milliseconds.
fn instant(text: &str) -> (String, u32) {
    let (clock, rest) = text.split_at(19);
    let milliseconds = rest
        .strip_prefix('.')
        .map_or(0, |fraction| fraction[..3].parse().expect("milliseconds"));
    (clock.to_owned(), milliseconds)
}
