pub const MAX_LINE_BYTES: usize = 512;
pub const MAX_ARGUMENTS: usize = 16;
pub const MAX_PIPELINE_COMMANDS: usize = 8;
pub const MAX_ENVIRONMENT_ENTRIES: usize = 16;
pub const MAX_ENVIRONMENT_NAME: usize = 32;
pub const MAX_ENVIRONMENT_VALUE: usize = 128;
pub const MAX_HISTORY_ENTRIES: usize = 16;
pub const MAX_HISTORY_LINE: usize = 128;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub start: u16,
    pub length: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputRedirect {
    pub path: Span,
    pub append: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command {
    arguments: [Span; MAX_ARGUMENTS],
    argument_count: u8,
    pub input: Option<Span>,
    pub output: Option<OutputRedirect>,
}

impl Command {
    const EMPTY: Self = Self {
        arguments: [Span {
            start: 0,
            length: 0,
        }; MAX_ARGUMENTS],
        argument_count: 0,
        input: None,
        output: None,
    };

    #[must_use]
    pub fn argument_count(&self) -> usize {
        usize::from(self.argument_count)
    }

    #[must_use]
    pub fn argument(&self, index: usize) -> Option<Span> {
        self.arguments
            .get(index)
            .copied()
            .filter(|_| index < self.argument_count())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedLine {
    storage: [u8; MAX_LINE_BYTES],
    storage_length: u16,
    commands: [Command; MAX_PIPELINE_COMMANDS],
    command_count: u8,
}

impl ParsedLine {
    const fn empty() -> Self {
        Self {
            storage: [0; MAX_LINE_BYTES],
            storage_length: 0,
            commands: [Command::EMPTY; MAX_PIPELINE_COMMANDS],
            command_count: 1,
        }
    }

    #[must_use]
    pub fn command_count(&self) -> usize {
        usize::from(self.command_count)
    }

    #[must_use]
    pub fn command(&self, index: usize) -> Option<&Command> {
        self.commands
            .get(index)
            .filter(|_| index < self.command_count())
    }

    #[must_use]
    pub fn bytes(&self, span: Span) -> &[u8] {
        let start = usize::from(span.start);
        let end = start.saturating_add(usize::from(span.length));
        self.storage.get(start..end).unwrap_or_default()
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), ParseError> {
        let index = usize::from(self.storage_length);
        if index == self.storage.len() {
            return Err(ParseError::ExpandedLineTooLong);
        }
        self.storage[index] = byte;
        self.storage_length += 1;
        Ok(())
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), ParseError> {
        for byte in bytes {
            self.push_byte(*byte)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    EmptyCommand,
    TooManyCommands,
    TooManyArguments,
    MissingRedirectTarget,
    DuplicateInputRedirect,
    DuplicateOutputRedirect,
    UnterminatedSingleQuote,
    UnterminatedDoubleQuote,
    TrailingEscape,
    ExpandedLineTooLong,
    InvalidEnvironmentName,
}

pub fn parse(line: &[u8], environment: &Environment) -> Result<ParsedLine, ParseError> {
    let mut parsed = ParsedLine::empty();
    let mut input_index = 0_usize;
    let mut command_index = 0_usize;

    while input_index < line.len() {
        skip_whitespace(line, &mut input_index);
        if input_index == line.len() {
            break;
        }

        match line[input_index] {
            b'|' => {
                if parsed.commands[command_index].argument_count == 0 {
                    return Err(ParseError::EmptyCommand);
                }
                command_index += 1;
                if command_index == MAX_PIPELINE_COMMANDS {
                    return Err(ParseError::TooManyCommands);
                }
                parsed.command_count += 1;
                input_index += 1;
            }
            b'<' => {
                input_index += 1;
                if parsed.commands[command_index].input.is_some() {
                    return Err(ParseError::DuplicateInputRedirect);
                }
                skip_whitespace(line, &mut input_index);
                if input_index == line.len() || is_operator(line[input_index]) {
                    return Err(ParseError::MissingRedirectTarget);
                }
                let target = parse_word(line, &mut input_index, environment, &mut parsed)?;
                parsed.commands[command_index].input = Some(target);
            }
            b'>' => {
                input_index += 1;
                let append = if line.get(input_index) == Some(&b'>') {
                    input_index += 1;
                    true
                } else {
                    false
                };
                if parsed.commands[command_index].output.is_some() {
                    return Err(ParseError::DuplicateOutputRedirect);
                }
                skip_whitespace(line, &mut input_index);
                if input_index == line.len() || is_operator(line[input_index]) {
                    return Err(ParseError::MissingRedirectTarget);
                }
                let path = parse_word(line, &mut input_index, environment, &mut parsed)?;
                parsed.commands[command_index].output = Some(OutputRedirect { path, append });
            }
            _ => {
                let argument = parse_word(line, &mut input_index, environment, &mut parsed)?;
                let command = &mut parsed.commands[command_index];
                let argument_index = usize::from(command.argument_count);
                if argument_index == MAX_ARGUMENTS {
                    return Err(ParseError::TooManyArguments);
                }
                command.arguments[argument_index] = argument;
                command.argument_count += 1;
            }
        }
    }

    if parsed.commands[command_index].argument_count == 0 {
        return Err(ParseError::EmptyCommand);
    }
    Ok(parsed)
}

fn parse_word(
    line: &[u8],
    input_index: &mut usize,
    environment: &Environment,
    parsed: &mut ParsedLine,
) -> Result<Span, ParseError> {
    let start = parsed.storage_length;
    let mut single_quote = false;
    let mut double_quote = false;
    let mut consumed = false;

    while *input_index < line.len() {
        let byte = line[*input_index];
        if !single_quote && !double_quote && (byte.is_ascii_whitespace() || is_operator(byte)) {
            break;
        }
        consumed = true;
        match byte {
            b'\'' if !double_quote => {
                single_quote = !single_quote;
                *input_index += 1;
            }
            b'"' if !single_quote => {
                double_quote = !double_quote;
                *input_index += 1;
            }
            b'\\' if !single_quote => {
                *input_index += 1;
                let escaped = *line.get(*input_index).ok_or(ParseError::TrailingEscape)?;
                parsed.push_byte(escaped)?;
                *input_index += 1;
            }
            b'$' if !single_quote => {
                *input_index += 1;
                let name_start = *input_index;
                while line
                    .get(*input_index)
                    .is_some_and(|byte| is_environment_name_byte(*byte))
                {
                    *input_index += 1;
                }
                if name_start == *input_index {
                    parsed.push_byte(b'$')?;
                } else if let Some(value) = environment.get(&line[name_start..*input_index]) {
                    parsed.push_bytes(value)?;
                }
            }
            _ => {
                parsed.push_byte(byte)?;
                *input_index += 1;
            }
        }
    }

    if single_quote {
        return Err(ParseError::UnterminatedSingleQuote);
    }
    if double_quote {
        return Err(ParseError::UnterminatedDoubleQuote);
    }
    if !consumed {
        return Err(ParseError::EmptyCommand);
    }
    Ok(Span {
        start,
        length: parsed.storage_length - start,
    })
}

fn skip_whitespace(line: &[u8], index: &mut usize) {
    while line.get(*index).is_some_and(u8::is_ascii_whitespace) {
        *index += 1;
    }
}

const fn is_operator(byte: u8) -> bool {
    matches!(byte, b'|' | b'<' | b'>')
}

const fn is_environment_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[derive(Clone, Copy)]
struct EnvironmentEntry {
    occupied: bool,
    name: [u8; MAX_ENVIRONMENT_NAME],
    name_length: u8,
    value: [u8; MAX_ENVIRONMENT_VALUE],
    value_length: u8,
}

impl EnvironmentEntry {
    const EMPTY: Self = Self {
        occupied: false,
        name: [0; MAX_ENVIRONMENT_NAME],
        name_length: 0,
        value: [0; MAX_ENVIRONMENT_VALUE],
        value_length: 0,
    };
}

pub struct Environment {
    entries: [EnvironmentEntry; MAX_ENVIRONMENT_ENTRIES],
}

impl Environment {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [EnvironmentEntry::EMPTY; MAX_ENVIRONMENT_ENTRIES],
        }
    }

    pub fn set(&mut self, name: &[u8], value: &[u8]) -> Result<(), EnvironmentError> {
        if !valid_environment_name(name) {
            return Err(EnvironmentError::InvalidName);
        }
        if value.len() > MAX_ENVIRONMENT_VALUE {
            return Err(EnvironmentError::ValueTooLong);
        }
        let existing = self.entries.iter().position(|entry| {
            entry.occupied && &entry.name[..usize::from(entry.name_length)] == name
        });
        let index = existing
            .or_else(|| self.entries.iter().position(|entry| !entry.occupied))
            .ok_or(EnvironmentError::Full)?;
        let entry = &mut self.entries[index];
        entry.name.fill(0);
        entry.value.fill(0);
        entry.name[..name.len()].copy_from_slice(name);
        entry.value[..value.len()].copy_from_slice(value);
        entry.name_length = u8::try_from(name.len()).map_err(|_| EnvironmentError::InvalidName)?;
        entry.value_length =
            u8::try_from(value.len()).map_err(|_| EnvironmentError::ValueTooLong)?;
        entry.occupied = true;
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        let entry = self.entries.iter().find(|entry| {
            entry.occupied && &entry.name[..usize::from(entry.name_length)] == name
        })?;
        Some(&entry.value[..usize::from(entry.value_length)])
    }

    pub fn unset(&mut self, name: &[u8]) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.occupied && &entry.name[..usize::from(entry.name_length)] == name)
        else {
            return false;
        };
        *entry = EnvironmentEntry::EMPTY;
        true
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|entry| entry.occupied).count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentError {
    InvalidName,
    ValueTooLong,
    Full,
}

fn valid_environment_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ENVIRONMENT_NAME
        && (name[0].is_ascii_alphabetic() || name[0] == b'_')
        && name[1..].iter().all(|byte| is_environment_name_byte(*byte))
}

#[derive(Clone, Copy)]
struct HistoryEntry {
    bytes: [u8; MAX_HISTORY_LINE],
    length: u8,
}

impl HistoryEntry {
    const EMPTY: Self = Self {
        bytes: [0; MAX_HISTORY_LINE],
        length: 0,
    };
}

pub struct History {
    entries: [HistoryEntry; MAX_HISTORY_ENTRIES],
    length: u8,
}

impl History {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [HistoryEntry::EMPTY; MAX_HISTORY_ENTRIES],
            length: 0,
        }
    }

    pub fn push(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        let length = line.len().min(MAX_HISTORY_LINE);
        if self
            .get(self.len().saturating_sub(1))
            .is_some_and(|previous| previous == &line[..length])
        {
            return;
        }
        if self.len() == MAX_HISTORY_ENTRIES {
            for index in 0..MAX_HISTORY_ENTRIES - 1 {
                self.entries[index] = self.entries[index + 1];
            }
            self.length -= 1;
        }
        let entry = &mut self.entries[self.len()];
        entry.bytes[..length].copy_from_slice(&line[..length]);
        entry.length = u8::try_from(length).unwrap_or(u8::MAX);
        self.length += 1;
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&[u8]> {
        let entry = self.entries.get(index).filter(|_| index < self.len())?;
        Some(&entry.bytes[..usize::from(entry.length)])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.length)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_expansion_pipeline_and_redirects() {
        let mut environment = Environment::new();
        environment.set(b"HOME", b"/home/nex").unwrap();
        let parsed = parse(
            br#"echo "hello world" '$HOME' $HOME | wc -c >> /tmp/count"#,
            &environment,
        )
        .unwrap();
        assert_eq!(parsed.command_count(), 2);
        let first = parsed.command(0).unwrap();
        assert_eq!(parsed.bytes(first.argument(0).unwrap()), b"echo");
        assert_eq!(parsed.bytes(first.argument(1).unwrap()), b"hello world");
        assert_eq!(parsed.bytes(first.argument(2).unwrap()), b"$HOME");
        assert_eq!(parsed.bytes(first.argument(3).unwrap()), b"/home/nex");
        let second = parsed.command(1).unwrap();
        assert_eq!(parsed.bytes(second.argument(0).unwrap()), b"wc");
        let redirect = second.output.unwrap();
        assert!(redirect.append);
        assert_eq!(parsed.bytes(redirect.path), b"/tmp/count");
    }

    #[test]
    fn rejects_empty_pipeline_and_missing_redirect() {
        let environment = Environment::new();
        assert_eq!(
            parse(b"echo ok | | wc", &environment),
            Err(ParseError::EmptyCommand)
        );
        assert_eq!(
            parse(b"cat <", &environment),
            Err(ParseError::MissingRedirectTarget)
        );
    }

    #[test]
    fn environment_replaces_and_unsets_values() {
        let mut environment = Environment::new();
        environment.set(b"PATH", b"/bin").unwrap();
        environment.set(b"PATH", b"/bin:/sbin").unwrap();
        assert_eq!(environment.get(b"PATH"), Some(&b"/bin:/sbin"[..]));
        assert!(environment.unset(b"PATH"));
        assert_eq!(environment.get(b"PATH"), None);
    }

    #[test]
    fn history_is_bounded_and_deduplicates_adjacent_lines() {
        let mut history = History::new();
        history.push(b"ls");
        history.push(b"ls");
        assert_eq!(history.len(), 1);
        for index in 0_u8..20 {
            history.push(&[b'a' + index]);
        }
        assert_eq!(history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(history.get(15), Some(&[b'a' + 19][..]));
    }
}
