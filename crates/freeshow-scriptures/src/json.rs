//! Bounded JSON.parse scalar semantics; finite decimal tokens use binary64 once.
//! No shared serde parser features are changed.
use crate::{Budget, Error, Result};
use serde_json::{Map, Number, Value};
enum Failure {
    Syntax,
    Resource(Error),
}
type Parsed<T> = std::result::Result<T, Failure>;
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Resource(error)
    }
}
struct Parser<'a, 'b> {
    text: &'a str,
    at: usize,
    budget: &'b mut Budget,
}
pub(crate) fn parse(text: &str, budget: &mut Budget) -> Result<Option<Value>> {
    let mut parser = Parser {
        text,
        at: 0,
        budget,
    };
    let result = (|| {
        let value = parser.value(0)?;
        parser.whitespace()?;
        if parser.at != text.len() {
            return Err(Failure::Syntax);
        }
        Ok(value)
    })();
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Failure::Syntax) => Ok(None),
        Err(Failure::Resource(error)) => Err(error),
    }
}
impl Parser<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn next(&mut self) -> Parsed<u8> {
        self.budget.check()?;
        let byte = self.peek().ok_or(Failure::Syntax)?;
        self.at += 1;
        Ok(byte)
    }
    fn whitespace(&mut self) -> Parsed<()> {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.next()?;
        }
        Ok(())
    }
    fn literal(&mut self, expected: &[u8]) -> Parsed<()> {
        for byte in expected {
            if self.next()? != *byte {
                return Err(Failure::Syntax);
            }
        }
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Parsed<Value> {
        self.whitespace()?;
        if depth > self.budget.limits.depth {
            return Err(Error::limit("Scripture JSON depth bound").into());
        }
        if self.budget.stats.parsed_nodes >= self.budget.limits.nodes {
            return Err(Error::limit("Scripture JSON node bound").into());
        }
        self.budget.stats.parsed_nodes += 1;
        self.budget.charge(256)?;
        match self.peek().ok_or(Failure::Syntax)? {
            b'"' => self.string().map(Value::String),
            b'n' => {
                self.literal(b"null")?;
                Ok(Value::Null)
            }
            b't' => {
                self.literal(b"true")?;
                Ok(Value::Bool(true))
            }
            b'f' => {
                self.literal(b"false")?;
                Ok(Value::Bool(false))
            }
            b'[' => {
                self.next()?;
                self.whitespace()?;
                let mut values = Vec::new();
                if self.peek() == Some(b']') {
                    self.next()?;
                    return Ok(Value::Array(values));
                }
                loop {
                    let value = self.value(depth + 1)?;
                    values.try_reserve(1).map_err(|_| {
                        Failure::Resource(Error::limit("Scripture array allocation"))
                    })?;
                    values.push(value);
                    self.whitespace()?;
                    match self.next()? {
                        b']' => return Ok(Value::Array(values)),
                        b',' => {}
                        _ => return Err(Failure::Syntax),
                    }
                }
            }
            b'{' => {
                self.next()?;
                self.whitespace()?;
                let mut values = Map::new();
                if self.peek() == Some(b'}') {
                    self.next()?;
                    return Ok(Value::Object(values));
                }
                loop {
                    self.whitespace()?;
                    if self.peek() != Some(b'"') {
                        return Err(Failure::Syntax);
                    }
                    let key = self.string()?;
                    self.whitespace()?;
                    if self.next()? != b':' {
                        return Err(Failure::Syntax);
                    }
                    let value = self.value(depth + 1)?;
                    values.insert(key, value);
                    self.whitespace()?;
                    match self.next()? {
                        b'}' => return Ok(Value::Object(values)),
                        b',' => {}
                        _ => return Err(Failure::Syntax),
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(Failure::Syntax),
        }
    }
    fn string(&mut self) -> Parsed<String> {
        let start = self.at;
        if self.next()? != b'"' {
            return Err(Failure::Syntax);
        }
        loop {
            if self.at - start > self.budget.limits.string_bytes {
                return Err(Error::limit("Scripture JSON string bound").into());
            }
            match self.next()? {
                b'"' => break,
                b'\\' => {
                    let escaped = self.next()?;
                    if escaped == b'u' {
                        let first = self.hex()?;
                        if (0xd800..=0xdbff).contains(&first) {
                            if self.peek() != Some(b'\\')
                                || self.text.as_bytes().get(self.at + 1) != Some(&b'u')
                            {
                                return Err(Error::unsupported("Lone JSON UTF-16 surrogate").into());
                            }
                            self.next()?;
                            self.next()?;
                            let second = self.hex()?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return Err(Error::unsupported("Lone JSON UTF-16 surrogate").into());
                            }
                        } else if (0xdc00..=0xdfff).contains(&first) {
                            return Err(Error::unsupported("Lone JSON UTF-16 surrogate").into());
                        }
                    } else if !matches!(
                        escaped,
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't'
                    ) {
                        return Err(Failure::Syntax);
                    }
                }
                0..=31 => return Err(Failure::Syntax),
                _ => {}
            }
        }
        let slice = &self.text[start..self.at];
        if slice.len() > self.budget.limits.string_bytes {
            return Err(Error::limit("Scripture JSON string bound").into());
        }
        self.budget.charge(slice.len().saturating_mul(2))?;
        serde_json::from_str(slice).map_err(|_| Failure::Syntax)
    }
    fn hex(&mut self) -> Parsed<u16> {
        let mut value = 0;
        for _ in 0..4 {
            let byte = self.next()?;
            let digit = char::from(byte).to_digit(16).ok_or(Failure::Syntax)?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }
    fn number(&mut self) -> Parsed<Value> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.next()?;
        }
        match self.next()? {
            b'0' => {}
            b'1'..=b'9' => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.next()?;
                }
            }
            _ => return Err(Failure::Syntax),
        }
        if self.peek() == Some(b'.') {
            self.next()?;
            let begin = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.next()?;
            }
            if begin == self.at {
                return Err(Failure::Syntax);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.next()?;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.next()?;
            }
            let begin = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.next()?;
            }
            if begin == self.at {
                return Err(Failure::Syntax);
            }
        }
        let number = self.text[start..self.at]
            .parse::<f64>()
            .map_err(|_| Failure::Syntax)?;
        let number = Number::from_f64(number).ok_or_else(|| {
            Failure::Resource(Error::unsupported("JSON number exceeds finite binary64"))
        })?;
        Ok(Value::Number(number))
    }
}
