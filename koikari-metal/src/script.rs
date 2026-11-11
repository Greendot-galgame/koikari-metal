//! Safe KAG parsing and a deliberately small, non-executable expression language.
//! Game scripts are data: no TJS engine, function calls, property accessors, or host APIs.
use anyhow::{Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub type Variables = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub args: BTreeMap<String, String>,
    /// KAG parameter macros such as `dr1`, `pc`, `s1`, and `fc`.
    pub flags: Vec<String>,
}
impl Tag {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.args.get(key).map(String::as_str)
    }
    pub fn flag(&self, key: &str) -> bool {
        self.flags.iter().any(|v| v == key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Text(String),
    Tag(Tag),
    Label(String),
    Script(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub line: usize,
    pub kind: TokenKind,
}

/// Parse attributes without splitting quoted spaces, `]`, `=`, or escaped quotes.
pub fn parse_tag(input: &str) -> Result<Tag> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let space = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    space(&mut i);
    let begin = i;
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    let name: String = chars[begin..i].iter().collect();
    ensure!(!name.is_empty(), "empty KAG tag");
    let mut result = Tag {
        name,
        args: BTreeMap::new(),
        flags: Vec::new(),
    };
    while i < chars.len() {
        space(&mut i);
        if i == chars.len() || chars[i] == ';' {
            break;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        ensure!(!key.is_empty(), "invalid attribute in {input:?}");
        space(&mut i);
        if i == chars.len() || chars[i] != '=' {
            result.flags.push(key);
            continue;
        }
        i += 1;
        space(&mut i);
        ensure!(i < chars.len(), "missing value for {key}");
        let mut value = String::new();
        if matches!(chars[i], '\'' | '"') {
            let quote = chars[i];
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if c == quote {
                    closed = true;
                    break;
                }
                if c == '\\' && i < chars.len() && (chars[i] == quote || chars[i] == '\\') {
                    value.push(chars[i]);
                    i += 1;
                } else {
                    value.push(c);
                }
            }
            ensure!(closed, "unterminated attribute {key}");
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                value.push(chars[i]);
                i += 1;
            }
        }
        // A shipped scenario contains `@t_move lay=@t_move lay=5`, a typo in the
        // original data. KAG stores attributes in a dictionary, so a repeated key
        // simply overwrites the earlier value; mirror that instead of failing the
        // whole scenario.
        result.args.insert(key.clone(), value);
    }
    Ok(result)
}

/// Line directives, labels, comments, literal `[[`, and bracket tags. Embedded TJS
/// is retained as one opaque Script token, never accidentally shown as dialogue.
pub fn tokenize(source: &str) -> Result<Vec<Token>> {
    ensure!(source.len() <= 32 * 1024 * 1024, "scenario exceeds 32 MiB");
    let mut out = Vec::new();
    let mut script: Option<(usize, String)> = None;
    for (index, raw) in source.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = index + 1;
        let s = raw.trim_start();
        if let Some((start, body)) = script.as_mut() {
            if s.eq_ignore_ascii_case("[endscript]") || s.eq_ignore_ascii_case("@endscript") {
                out.push(Token {
                    line: *start,
                    kind: TokenKind::Script(body.clone()),
                });
                script = None;
            } else {
                body.push_str(raw);
                body.push('\n');
            }
            continue;
        }
        if s.is_empty() || s.starts_with(';') {
            continue;
        }
        if let Some(label) = s.strip_prefix('*') {
            out.push(Token {
                line,
                kind: TokenKind::Label(label.split('|').next().unwrap_or("").trim().to_owned()),
            });
            continue;
        }
        if let Some(raw_tag) = s.strip_prefix('@') {
            let tag = parse_tag(raw_tag)?;
            if tag.name.eq_ignore_ascii_case("iscript") {
                script = Some((line, String::new()));
            } else {
                out.push(Token {
                    line,
                    kind: TokenKind::Tag(tag),
                });
            }
            continue;
        }
        let chars: Vec<char> = s.chars().collect();
        let mut text = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '[' {
                text.push(chars[i]);
                i += 1;
                continue;
            }
            if chars.get(i + 1) == Some(&'[') {
                text.push('[');
                i += 2;
                continue;
            }
            if !text.is_empty() {
                out.push(Token {
                    line,
                    kind: TokenKind::Text(std::mem::take(&mut text)),
                });
            }
            i += 1;
            let start = i;
            let mut quote = None;
            let mut escaped = false;
            while i < chars.len() {
                let c = chars[i];
                if escaped {
                    escaped = false;
                    i += 1;
                    continue;
                }
                if quote.is_some() && c == '\\' {
                    escaped = true;
                    i += 1;
                    continue;
                }
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == ']' {
                    break;
                }
                i += 1;
            }
            ensure!(i < chars.len(), "unterminated tag at line {line}");
            let tag = parse_tag(&chars[start..i].iter().collect::<String>())?;
            i += 1;
            if tag.name.eq_ignore_ascii_case("iscript") {
                ensure!(
                    i == chars.len(),
                    "inline iscript bodies are unsupported at line {line}"
                );
                script = Some((line, String::new()));
            } else {
                out.push(Token {
                    line,
                    kind: TokenKind::Tag(tag),
                });
            }
        }
        if !text.is_empty() {
            out.push(Token {
                line,
                kind: TokenKind::Text(text),
            });
        }
    }
    ensure!(script.is_none(), "unterminated iscript block");
    Ok(out)
}

#[derive(Clone, Debug)]
pub enum InstructionKind {
    Text(String),
    Command(Tag),
    Branch { expression: String, false_pc: usize },
    JumpPc(usize),
    Unsupported(String),
}
#[derive(Clone, Debug)]
pub struct Instruction {
    pub line: usize,
    pub kind: InstructionKind,
}
#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub instructions: Vec<Instruction>,
    pub labels: BTreeMap<String, usize>,
    pub duplicate_labels: BTreeSet<String>,
    /// Stable FNV-1a content identity (a change detector, not an authenticity hash).
    pub fingerprint: String,
}
impl Program {
    pub fn compile(name: impl Into<String>, source: &str) -> Result<Self> {
        struct IfFrame {
            pending: Option<usize>,
            exits: Vec<usize>,
            had_else: bool,
        }
        let mut result = Self {
            name: name.into(),
            instructions: Vec::new(),
            labels: BTreeMap::new(),
            duplicate_labels: BTreeSet::new(),
            fingerprint: fingerprint(source),
        };
        let mut stack: Vec<IfFrame> = Vec::new();
        for token in tokenize(source)? {
            let line = token.line;
            let kind = match token.kind {
                TokenKind::Label(label) => {
                    if result.labels.contains_key(&label) {
                        result.duplicate_labels.insert(label);
                    } else {
                        result.labels.insert(label, result.instructions.len());
                    }
                    continue;
                }
                TokenKind::Text(text) => InstructionKind::Text(text),
                TokenKind::Script(_) => {
                    InstructionKind::Unsupported("embedded TJS iscript block".to_owned())
                }
                TokenKind::Tag(tag) => match tag.name.to_ascii_lowercase().as_str() {
                    "if" => {
                        let pc = result.instructions.len();
                        stack.push(IfFrame {
                            pending: Some(pc),
                            exits: Vec::new(),
                            had_else: false,
                        });
                        InstructionKind::Branch {
                            expression: tag
                                .get("exp")
                                .ok_or_else(|| anyhow!("if without exp at {line}"))?
                                .to_owned(),
                            false_pc: usize::MAX,
                        }
                    }
                    "elsif" | "elseif" | "else" => {
                        let frame = stack
                            .last_mut()
                            .ok_or_else(|| anyhow!("unmatched else at {line}"))?;
                        ensure!(!frame.had_else, "else after else at {line}");
                        let jump = result.instructions.len();
                        result.instructions.push(Instruction {
                            line,
                            kind: InstructionKind::JumpPc(usize::MAX),
                        });
                        frame.exits.push(jump);
                        let next = result.instructions.len();
                        if let Some(pc) = frame.pending.take() {
                            patch_branch(&mut result.instructions[pc], next);
                        }
                        if tag.name.eq_ignore_ascii_case("else") {
                            frame.had_else = true;
                            continue;
                        }
                        frame.pending = Some(next);
                        InstructionKind::Branch {
                            expression: tag
                                .get("exp")
                                .ok_or_else(|| anyhow!("elsif without exp"))?
                                .to_owned(),
                            false_pc: usize::MAX,
                        }
                    }
                    "endif" => {
                        let frame = stack
                            .pop()
                            .ok_or_else(|| anyhow!("unmatched endif at {line}"))?;
                        let end = result.instructions.len();
                        if let Some(pc) = frame.pending {
                            patch_branch(&mut result.instructions[pc], end);
                        }
                        for pc in frame.exits {
                            result.instructions[pc].kind = InstructionKind::JumpPc(end);
                        }
                        continue;
                    }
                    "macro" | "endmacro" | "pmacro" | "while" | "endwhile" => {
                        InstructionKind::Unsupported(format!("definition/loop tag {}", tag.name))
                    }
                    _ => InstructionKind::Command(tag),
                },
            };
            result.instructions.push(Instruction { line, kind });
        }
        ensure!(stack.is_empty(), "unclosed if in {}", result.name);
        Ok(result)
    }
    pub fn label_pc(&self, label: &str) -> Result<usize> {
        let label = label.trim_start_matches('*');
        ensure!(
            !self.duplicate_labels.contains(label),
            "ambiguous duplicate label {label} in {}",
            self.name
        );
        self.labels
            .get(label)
            .copied()
            .ok_or_else(|| anyhow!("label {label} not found in {}", self.name))
    }
}
fn patch_branch(op: &mut Instruction, dest: usize) {
    if let InstructionKind::Branch { false_pc, .. } = &mut op.kind {
        *false_pc = dest;
    }
}
pub fn fingerprint(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for b in source.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[derive(Clone, Debug, PartialEq)]
enum Lex {
    Number(i64),
    String(String),
    Ident(String),
    Op(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semi,
    End,
}
fn lex(source: &str) -> Result<Vec<Lex>> {
    let c: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        ensure!(out.len() < 65536, "expression token budget exceeded");
        if c[i].is_whitespace() {
            i += 1;
            continue;
        }
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            ensure!(i + 1 < c.len(), "unterminated expression comment");
            i += 2;
            continue;
        }
        if c[i] == '\'' || c[i] == '"' {
            let q = c[i];
            i += 1;
            let mut s = String::new();
            let mut closed = false;
            while i < c.len() {
                let ch = c[i];
                i += 1;
                if ch == q {
                    closed = true;
                    break;
                }
                if ch == '\\' {
                    ensure!(i < c.len(), "incomplete string escape");
                    let ch = c[i];
                    i += 1;
                    s.push(match ch {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        _ => ch,
                    });
                } else {
                    s.push(ch);
                }
            }
            ensure!(closed, "unterminated expression string");
            out.push(Lex::String(s));
            continue;
        }
        if c[i].is_ascii_digit() {
            let start = i;
            i += 1;
            if c[start] == '0' && c.get(i).is_some_and(|v| *v == 'x' || *v == 'X') {
                i += 1;
                let digits = i;
                while i < c.len() && c[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = c[digits..i].iter().collect();
                out.push(Lex::Number(i64::from_str_radix(&text, 16)?));
            } else {
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = c[start..i].iter().collect();
                out.push(Lex::Number(text.parse()?));
            }
            continue;
        }
        if c[i].is_alphabetic() || c[i] == '_' || c[i] == '$' {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || matches!(c[i], '_' | '.' | '$')) {
                i += 1;
            }
            out.push(Lex::Ident(c[start..i].iter().collect()));
            continue;
        }
        let ch = c[i];
        i += 1;
        let special = match ch {
            '(' => Some(Lex::LParen),
            ')' => Some(Lex::RParen),
            '[' => Some(Lex::LBracket),
            ']' => Some(Lex::RBracket),
            ',' => Some(Lex::Comma),
            ';' => Some(Lex::Semi),
            _ => None,
        };
        if let Some(token) = special {
            out.push(token);
            continue;
        }
        ensure!(
            "+-*/%!=<>&|".contains(ch),
            "unsupported expression character {ch:?}"
        );
        let mut op = String::from(ch);
        if i < c.len() {
            let pair = format!("{ch}{}", c[i]);
            if [
                "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "+=", "-=", "++", "--",
            ]
            .contains(&pair.as_str())
            {
                op = pair;
                i += 1;
                if (op == "==" || op == "!=") && c.get(i) == Some(&'=') {
                    op.push('=');
                    i += 1;
                }
            }
        }
        out.push(Lex::Op(op));
    }
    out.push(Lex::End);
    Ok(out)
}
struct Parser<'a> {
    tokens: Vec<Lex>,
    pos: usize,
    vars: &'a Variables,
}
impl<'a> Parser<'a> {
    fn peek(&self) -> &Lex {
        &self.tokens[self.pos]
    }
    fn next(&mut self) -> Lex {
        let item = self.tokens[self.pos].clone();
        self.pos += 1;
        item
    }
    fn expr(&mut self, min: u8, depth: usize) -> Result<Value> {
        ensure!(depth < 64, "expression nesting budget exceeded");
        let mut left = match self.next() {
            Lex::Number(n) => json!(n),
            Lex::String(s) => Value::String(s),
            Lex::Ident(s) => match s.as_str() {
                "true" => json!(true),
                "false" => json!(false),
                "void" | "null" => Value::Null,
                _ => self
                    .vars
                    .get(&s)
                    .cloned()
                    .ok_or_else(|| anyhow!("unresolved variable {s}"))?,
            },
            Lex::Op(op) if ["!", "-", "+", "&"].contains(&op.as_str()) => {
                let v = self.expr(20, depth + 1)?;
                match op.as_str() {
                    "!" => json!(!truthy(&v)?),
                    "-" => json!(
                        integer(&v)?
                            .checked_neg()
                            .ok_or_else(|| anyhow!("integer overflow"))?
                    ),
                    "+" => json!(integer(&v)?),
                    _ => v,
                }
            }
            Lex::LParen => {
                let v = self.expr(0, depth + 1)?;
                ensure!(self.next() == Lex::RParen, "missing )");
                v
            }
            Lex::LBracket => {
                let mut a = Vec::new();
                while self.peek() != &Lex::RBracket {
                    ensure!(a.len() < 16384, "array literal budget exceeded");
                    a.push(self.expr(0, depth + 1)?);
                    if self.peek() == &Lex::Comma {
                        self.next();
                    } else {
                        break;
                    }
                }
                ensure!(self.next() == Lex::RBracket, "missing ]");
                Value::Array(a)
            }
            other => bail!("expected a safe expression, got {other:?}"),
        };
        loop {
            let Lex::Op(op) = self.peek() else {
                break;
            };
            let precedence = match op.as_str() {
                "||" => 1,
                "&&" => 2,
                "==" | "!=" | "===" | "!==" => 3,
                "<" | ">" | "<=" | ">=" => 4,
                "<<" | ">>" => 5,
                "+" | "-" => 6,
                "*" | "/" | "%" => 7,
                _ => break,
            };
            if precedence < min {
                break;
            }
            let op = op.clone();
            self.next();
            let right = self.expr(precedence + 1, depth + 1)?;
            left = binary(&op, left, right)?;
        }
        Ok(left)
    }
}
fn integer(v: &Value) -> Result<i64> {
    v.as_i64()
        .ok_or_else(|| anyhow!("expected integer, got {v}"))
}
pub fn truthy(v: &Value) -> Result<bool> {
    Ok(match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_i64().ok_or_else(|| anyhow!("noninteger condition"))? != 0,
        Value::String(v) => !v.is_empty(),
        _ => bail!("non-scalar condition"),
    })
}
fn binary(op: &str, a: Value, b: Value) -> Result<Value> {
    Ok(match op {
        "==" | "===" => json!(a == b),
        "!=" | "!==" => json!(a != b),
        "&&" => json!(truthy(&a)? && truthy(&b)?),
        "||" => json!(truthy(&a)? || truthy(&b)?),
        "+" if a.is_string() || b.is_string() => {
            Value::String(format!("{}{}", display_value(&a), display_value(&b)))
        }
        _ => {
            let (a, b) = (integer(&a)?, integer(&b)?);
            match op {
                "<" => json!(a < b),
                ">" => json!(a > b),
                "<=" => json!(a <= b),
                ">=" => json!(a >= b),
                "+" => json!(
                    a.checked_add(b)
                        .ok_or_else(|| anyhow!("integer overflow"))?
                ),
                "-" => json!(
                    a.checked_sub(b)
                        .ok_or_else(|| anyhow!("integer overflow"))?
                ),
                "*" => json!(
                    a.checked_mul(b)
                        .ok_or_else(|| anyhow!("integer overflow"))?
                ),
                "/" => json!(
                    a.checked_div(b)
                        .ok_or_else(|| anyhow!("division by zero or overflow"))?
                ),
                "%" => json!(
                    a.checked_rem(b)
                        .ok_or_else(|| anyhow!("remainder by zero or overflow"))?
                ),
                "<<" | ">>" => {
                    ensure!((0..63).contains(&b), "invalid shift");
                    json!(if op == "<<" {
                        a.checked_mul(1i64 << b)
                            .ok_or_else(|| anyhow!("shift overflow"))?
                    } else {
                        a >> b
                    })
                }
                _ => bail!("unsupported operator {op}"),
            }
        }
    })
}
pub fn display_value(v: &Value) -> String {
    if let Value::String(s) = v {
        s.clone()
    } else {
        v.to_string()
    }
}
pub fn evaluate(expression: &str, vars: &Variables) -> Result<Value> {
    let mut parser = Parser {
        tokens: lex(expression)?,
        pos: 0,
        vars,
    };
    let value = parser.expr(0, 0)?;
    ensure!(
        parser.peek() == &Lex::End,
        "unsupported expression suffix {:?} (calls and indexing are not executable)",
        parser.peek()
    );
    Ok(value)
}
/// Atomic assignments to plain `f.*`, `tf.*`, `sf.*` variables only.
pub fn assign(expression: &str, variables: &mut Variables) -> Result<()> {
    let tokens = lex(expression)?;
    let mut candidate = variables.clone();
    let mut pos = 0;
    while tokens[pos] != Lex::End {
        let Lex::Ident(name) = &tokens[pos] else {
            bail!("assignment must start with a variable");
        };
        let name = name.clone();
        pos += 1;
        ensure!(
            ["f.", "tf.", "sf."].iter().any(|p| name.starts_with(p))
                && name.split('.').all(|p| !p.is_empty()),
            "write to non-game variable {name} is forbidden"
        );
        let Lex::Op(op) = &tokens[pos] else {
            bail!("missing assignment operator");
        };
        let op = op.clone();
        pos += 1;
        let value = if op == "++" || op == "--" {
            let old = candidate
                .get(&name)
                .ok_or_else(|| anyhow!("unresolved variable {name}"))?
                .clone();
            binary(if op == "++" { "+" } else { "-" }, old, json!(1))?
        } else {
            ensure!(
                ["=", "+=", "-="].contains(&op.as_str()),
                "unsupported assignment {op}"
            );
            let mut parser = Parser {
                tokens: tokens.clone(),
                pos,
                vars: &candidate,
            };
            let value = parser.expr(0, 0)?;
            pos = parser.pos;
            if op == "=" {
                value
            } else {
                binary(
                    if op == "+=" { "+" } else { "-" },
                    candidate
                        .get(&name)
                        .ok_or_else(|| anyhow!("unresolved variable {name}"))?
                        .clone(),
                    value,
                )?
            }
        };
        candidate.insert(name, value);
        if tokens[pos] == Lex::Semi {
            pos += 1;
        } else {
            ensure!(
                tokens[pos] == Lex::End,
                "unsupported assignment suffix {:?}",
                tokens[pos]
            );
        }
    }
    *variables = candidate;
    Ok(())
}

/// Read only a literal array assignment from shipped metadata; surrounding TJS
/// never runs. Comments and integer shifts in the array are accepted.
pub fn literal_assignment(source: &str, name: &str) -> Result<Value> {
    let begin = source
        .find(name)
        .ok_or_else(|| anyhow!("metadata {name} absent"))?
        + name.len();
    let rest = source[begin..].trim_start();
    let rest = rest
        .strip_prefix('=')
        .ok_or_else(|| anyhow!("metadata {name} is not an assignment"))?;
    let empty = Variables::new();
    // Bound acquisition to the first balanced array, not the rest of the TJS file.
    let rest = rest.trim_start();
    let end = array_end(rest)?;
    let mut parser = Parser {
        tokens: lex(&rest[..end])?,
        pos: 0,
        vars: &empty,
    };
    let value = parser.expr(0, 0)?;
    ensure!(
        parser.peek() == &Lex::End && value.is_array(),
        "metadata {name} is not a literal array"
    );
    Ok(value)
}
fn array_end(source: &str) -> Result<usize> {
    ensure!(source.starts_with('['), "expected metadata array");
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    for (i, c) in source.char_indices() {
        if comment {
            if c == '\n' {
                comment = false;
            } else {
                continue;
            }
        }
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '/' && source[i..].starts_with("//") {
            comment = true;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '[' {
            depth += 1;
        } else if c == ']' {
            depth -= 1;
            if depth == 0 {
                return Ok(i + 1);
            }
        }
    }
    bail!("unterminated metadata array")
}
