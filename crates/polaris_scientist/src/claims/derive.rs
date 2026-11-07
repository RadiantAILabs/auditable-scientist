//! Recomputing a recorded derivation.
//!
//! A claim may record how a number it states was computed from recorded
//! values: an arithmetic `expression` over named `inputs`, each input naming a
//! recorded source. The check evaluates the expression itself, so a derived
//! number is backed only when the arithmetic really gives it.
//!
//! The expression language is small on purpose: decimal numbers, input names,
//! `+ - * /`, parentheses, a leading minus, and the functions `abs`, `sqrt`,
//! `min` and `max`.

use std::collections::BTreeMap;

/// Why an expression could not be evaluated.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeriveError {
    /// The text is not an expression of the language.
    #[error("the expression is not valid: {0}")]
    Syntax(String),
    /// The expression names an input the derivation does not declare.
    #[error("the expression uses `{0}`, which is not one of its inputs")]
    UnknownInput(String),
    /// The arithmetic has no finite result, such as a division by zero.
    #[error("the arithmetic has no finite result")]
    NotFinite,
}

/// Evaluates `expression` with the named `inputs`.
///
/// # Errors
///
/// Returns [`DeriveError`] when the expression is malformed, uses a name that
/// is not an input, or has no finite value.
pub fn evaluate(expression: &str, inputs: &BTreeMap<String, f64>) -> Result<f64, DeriveError> {
    let mut parser = Parser {
        chars: expression.chars().collect(),
        at: 0,
        inputs,
    };
    let value = parser.sum()?;
    parser.blanks();
    if parser.at < parser.chars.len() {
        return Err(DeriveError::Syntax(format!(
            "unexpected `{}`",
            parser.chars[parser.at]
        )));
    }
    if value.is_finite() {
        Ok(value)
    } else {
        Err(DeriveError::NotFinite)
    }
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    inputs: &'a BTreeMap<String, f64>,
}

impl Parser<'_> {
    fn blanks(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.blanks();
        self.chars.get(self.at).copied()
    }

    fn sum(&mut self) -> Result<f64, DeriveError> {
        let mut value = self.product()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.at += 1;
            let rhs = self.product()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    fn product(&mut self) -> Result<f64, DeriveError> {
        let mut value = self.unary()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.at += 1;
            let rhs = self.unary()?;
            value = if op == '*' { value * rhs } else { value / rhs };
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<f64, DeriveError> {
        match self.peek() {
            Some('-') => {
                self.at += 1;
                Ok(-self.unary()?)
            }
            Some('+') => {
                self.at += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<f64, DeriveError> {
        match self.peek() {
            Some('(') => {
                self.at += 1;
                let value = self.sum()?;
                self.expect(')')?;
                Ok(value)
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_alphabetic() || c == '_' => self.name(),
            Some(c) => Err(DeriveError::Syntax(format!("unexpected `{c}`"))),
            None => Err(DeriveError::Syntax("the expression ends too soon".into())),
        }
    }

    fn expect(&mut self, close: char) -> Result<(), DeriveError> {
        if self.peek() == Some(close) {
            self.at += 1;
            Ok(())
        } else {
            Err(DeriveError::Syntax(format!("expected `{close}`")))
        }
    }

    fn number(&mut self) -> Result<f64, DeriveError> {
        let start = self.at;
        while self
            .chars
            .get(self.at)
            .is_some_and(|c| c.is_ascii_digit() || *c == '.')
        {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        text.parse::<f64>()
            .map_err(|_| DeriveError::Syntax(format!("`{text}` is not a number")))
    }

    fn name(&mut self) -> Result<f64, DeriveError> {
        let start = self.at;
        while self
            .chars
            .get(self.at)
            .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        {
            self.at += 1;
        }
        let name: String = self.chars[start..self.at].iter().collect();
        if self.peek() == Some('(') {
            self.at += 1;
            let mut args = vec![self.sum()?];
            while self.peek() == Some(',') {
                self.at += 1;
                args.push(self.sum()?);
            }
            self.expect(')')?;
            return match (name.as_str(), args.as_slice()) {
                ("abs", [x]) => Ok(x.abs()),
                ("sqrt", [x]) => Ok(x.sqrt()),
                ("min", [first, rest @ ..]) => Ok(rest.iter().copied().fold(*first, f64::min)),
                ("max", [first, rest @ ..]) => Ok(rest.iter().copied().fold(*first, f64::max)),
                _ => Err(DeriveError::Syntax(format!(
                    "`{name}` with {} argument(s) is not a known function",
                    args.len()
                ))),
            };
        }
        self.inputs
            .get(&name)
            .copied()
            .ok_or(DeriveError::UnknownInput(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(expression: &str) -> Result<f64, DeriveError> {
        let inputs = BTreeMap::from([("a".to_string(), 2.6298), ("b".to_string(), 0.9598)]);
        evaluate(expression, &inputs)
    }

    #[test]
    fn arithmetic_follows_the_usual_order() {
        assert!((run("a / b").unwrap() - 2.6298 / 0.9598).abs() < 1e-12);
        assert!(
            (run("100 * (a - b) / b").unwrap() - 100.0 * (2.6298 - 0.9598) / 0.9598).abs() < 1e-9
        );
        assert_eq!(run("2 + 3 * 4").unwrap(), 14.0);
        assert_eq!(run("-(1 + 2)").unwrap(), -3.0);
    }

    #[test]
    fn functions_work_on_inputs() {
        assert_eq!(run("abs(b - a)").unwrap(), (0.9598f64 - 2.6298).abs());
        assert_eq!(run("max(a, b, 1)").unwrap(), 2.6298);
        assert_eq!(run("min(a, b)").unwrap(), 0.9598);
        assert_eq!(run("sqrt(4)").unwrap(), 2.0);
    }

    #[test]
    fn bad_expressions_say_why() {
        assert!(matches!(run("a +"), Err(DeriveError::Syntax(_))));
        assert!(matches!(run("a b"), Err(DeriveError::Syntax(_))));
        assert_eq!(run("a / 0"), Err(DeriveError::NotFinite));
        assert_eq!(
            run("a + c"),
            Err(DeriveError::UnknownInput("c".to_string()))
        );
        assert!(matches!(run("log(a)"), Err(DeriveError::Syntax(_))));
    }
}
