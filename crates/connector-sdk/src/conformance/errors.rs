//! Errors: each failing request's kind and retry flag, and that no error the
//! suite receives reveals a secret option value or a filter literal.

use std::{
    collections::BTreeSet,
    sync::{Mutex, MutexGuard, PoisonError},
};

use substrait::proto::{
    Expression, Plan, Rel, expression, expression::literal::LiteralType, function_argument,
    plan_rel, rel,
};

use super::{ErrorCase, ExpectedError, FailingRequest, Fixture, Probe, assert_case, read};
use crate::{ConnectorDescriptor, ConnectorError, DiagnosticValue, SecretString};

pub(super) fn check<F: Fixture>(probe: &Probe<'_, F>) {
    for case in probe.fixture.error_cases() {
        let ErrorCase {
            name,
            options,
            request,
            expected,
        } = case;
        let error = match request {
            FailingRequest::OpenSession => probe.session_with(options).err(),
            FailingRequest::Read(offered) => probe
                .session_with(options)
                .and_then(|session| read::read(probe, &session, &offered))
                .err(),
        };
        let error = error.unwrap_or_else(|| panic!("{name}: the request succeeds"));
        let actual = ExpectedError {
            kind: error.kind(),
            retryable: error.retryable(),
        };
        assert_case(name, &actual, &expected);
    }
}

/// Every error the suite received, with the values none of them may reveal.
pub(super) struct Observed {
    secret_keys: BTreeSet<String>,
    state: Mutex<ObservedState>,
}

#[derive(Default)]
struct ObservedState {
    secrets: BTreeSet<String>,
    /// Each error with the filter literals of the request that returned it.
    errors: Vec<(ConnectorError, Vec<String>)>,
}

impl Observed {
    pub(super) fn new(descriptor: &ConnectorDescriptor) -> Self {
        Self {
            secret_keys: descriptor
                .options
                .iter()
                .filter(|option| option.secret)
                .map(|option| option.key.clone())
                .collect(),
            state: Mutex::default(),
        }
    }

    /// A panicking check fails the run, so a poisoned lock holds nothing stale.
    fn lock(&self) -> MutexGuard<'_, ObservedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the secret values among connection options about to be used.
    pub(super) fn secrets(&self, options: &[(String, SecretString)]) {
        let mut state = self.lock();
        for (key, value) in options {
            if self.secret_keys.contains(key) {
                state.secrets.insert(value.expose().to_owned());
            }
        }
    }

    /// Records the error of `result`, returned by a request whose plan
    /// carries `literals`.
    pub(super) fn result<T>(
        &self,
        result: Result<T, ConnectorError>,
        literals: &[String],
    ) -> Result<T, ConnectorError> {
        if let Err(error) = &result {
            self.lock().errors.push((error.clone(), literals.to_vec()));
        }
        result
    }

    /// Fails when an error's message, Data Source message, or diagnostics
    /// contain a secret value or a literal of its request.
    pub(super) fn check(&self) {
        let state = self.lock();
        for (error, literals) in &state.errors {
            let mut texts = vec![
                error.to_string(),
                error.data_source_message().unwrap_or_default().to_owned(),
            ];
            texts.extend(error.diagnostics().iter().map(|entry| match entry.value() {
                DiagnosticValue::Utf8(text) => text.clone(),
                DiagnosticValue::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                other => format!("{other:?}"),
            }));
            for hidden in state.secrets.iter().chain(literals) {
                for text in &texts {
                    assert!(
                        !text.contains(hidden.as_str()),
                        "a {:?} error reveals a secret value or filter literal",
                        error.kind()
                    );
                }
            }
        }
    }
}

/// The text of every string literal in the plan's filters.
pub(super) fn filter_literals(plan: &Plan) -> Vec<String> {
    let mut literals = Vec::new();
    for relation in &plan.relations {
        if let Some(plan_rel::RelType::Root(root)) = &relation.rel_type {
            if let Some(input) = &root.input {
                relation_literals(input, &mut literals);
            }
        }
    }
    literals
}

fn relation_literals(relation: &Rel, literals: &mut Vec<String>) {
    match &relation.rel_type {
        Some(rel::RelType::Filter(filter)) => {
            if let Some(condition) = &filter.condition {
                expression_literals(condition, literals);
            }
            if let Some(input) = &filter.input {
                relation_literals(input, literals);
            }
        }
        Some(rel::RelType::Project(project)) => {
            if let Some(input) = &project.input {
                relation_literals(input, literals);
            }
        }
        _ => {}
    }
}

fn expression_literals(expression: &Expression, literals: &mut Vec<String>) {
    match &expression.rex_type {
        Some(expression::RexType::Literal(literal)) => literal_text(literal, literals),
        Some(expression::RexType::ScalarFunction(function)) => {
            for argument in &function.arguments {
                if let Some(function_argument::ArgType::Value(value)) = &argument.arg_type {
                    expression_literals(value, literals);
                }
            }
        }
        _ => {}
    }
}

fn literal_text(literal: &expression::Literal, literals: &mut Vec<String>) {
    match &literal.literal_type {
        Some(LiteralType::String(text) | LiteralType::FixedChar(text)) => {
            literals.push(text.clone());
        }
        Some(LiteralType::VarChar(text)) => literals.push(text.value.clone()),
        Some(LiteralType::UserDefined(user_defined)) => {
            if let Some(expression::literal::user_defined::Val::Struct(carrier)) = &user_defined.val
            {
                for field in &carrier.fields {
                    literal_text(field, literals);
                }
            }
        }
        _ => {}
    }
}
