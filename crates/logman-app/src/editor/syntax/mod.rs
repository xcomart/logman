//! What a file *is*, and the line-at-a-time lexers that colour it.
//!
//! # Terminal-level highlighting, not a parser
//!
//! Every lexer here is a hand-written state machine over bytes, and none of
//! them builds a tree. What the editor needs is what a good `cat` would give
//! you: a comment is grey, a string is green, the left-hand side of a mapping
//! stands out from the right. A `.yml` that is invalid YAML still has to be
//! readable *while it is being fixed*, which is the argument against a real
//! parser as much as the size of one is: a parser tells you the document is
//! wrong, and a scanner just keeps colouring. So the rule every lexer here is
//! held to is that it never panics and never refuses — a line of random bytes
//! comes out as one plain run, not as an error.
//!
//! # Incremental by construction
//!
//! [`lex_line`] takes the state the *previous* line ended in and hands back the
//! state this one ends in. That is what makes an edit cost one line instead of
//! a file: [`crate::editor::highlight`] caches one [`LineState`] per line, and
//! after an edit it re-lexes downwards only until the end states stop moving.
//! [`LineState`] is `Copy + Eq` for exactly that comparison, and it is a fixed
//! size — a heredoc tag longer than [`TAG_LIMIT`] is the one thing that does
//! not fit, and it is dropped rather than allowed to grow the state.
//!
//! # Tokens tile the line
//!
//! Every lexer returns spans that cover the whole line, in order, with no gaps
//! and no overlaps, and every boundary is a `char` boundary. The renderer turns
//! them straight into shaping runs, and a run that ended mid-character would
//! take the text system down with it. [`Runs`] is what makes that true by
//! construction rather than by care: a lexer only ever says "this span is
//! interesting", and the gaps between become plain runs on their own.

pub mod conf;
pub mod dockerfile;
pub mod json;
pub mod shell;
pub mod toml;
pub mod yaml;

/// What a lexer decided a span of bytes is.
///
/// Deliberately short. These are the distinctions that survive being applied to
/// six formats at once — a comment is a comment in all of them, and a `Key` is
/// the left of a mapping whether the mapping is written `a: b`, `a = b` or
/// `[section]`. Operators and punctuation are absent because colouring them is
/// what makes a terminal-level scheme look busy rather than legible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Whatever the lexer had no opinion about, drawn in the foreground.
    Plain,
    /// To the end of the line, or the whole of a block comment's line.
    Comment,
    /// Quoted text, including its quotes.
    String,
    /// A numeric literal, and anything dotted or colon-separated that starts
    /// like one: a version, an address, a timestamp.
    Number,
    /// A word the format reserves.
    Keyword,
    /// The left-hand side of a mapping, or a section header.
    Key,
    /// A shell-style expansion: `$NAME`, `${...}`, a YAML anchor or alias.
    Variable,
    /// `true`, `false`, `null` and the spellings each format allows.
    Literal,
}

/// A classified span of one line, in bytes from the start of that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// First byte, inclusive.
    pub start: usize,
    /// Last byte, exclusive.
    pub end: usize,
}

/// The longest heredoc tag a [`LineState`] can carry.
///
/// The state has to be `Copy` and a fixed size — there is one per line of the
/// file — so the tag lives inline. Sixteen bytes covers `EOF`, `SQL`,
/// `PYTHON_SCRIPT` and every tag anybody actually writes; a longer one means
/// the heredoc is not tracked at all and its body is lexed as ordinary shell,
/// which is wrong in colour only.
const TAG_LIMIT: usize = 16;

/// An open shell heredoc: the tag that will close it, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heredoc {
    /// The tag, `len` bytes of it.
    tag: [u8; TAG_LIMIT],
    /// How much of `tag` is the tag.
    len: u8,
    /// `<<-` rather than `<<`, which lets the terminator be indented.
    dash: bool,
}

impl Heredoc {
    /// A heredoc closed by `tag`, or `None` when the tag will not fit.
    pub fn new(tag: &str, dash: bool) -> Option<Self> {
        if tag.is_empty() || tag.len() > TAG_LIMIT {
            return None;
        }
        let mut bytes = [0; TAG_LIMIT];
        bytes[..tag.len()].copy_from_slice(tag.as_bytes());
        Some(Self {
            tag: bytes,
            len: tag.len() as u8,
            dash,
        })
    }

    /// Whether `line` is the terminator.
    ///
    /// Trailing whitespace is forgiven and leading whitespace is forgiven only
    /// for `<<-`, which is roughly what a shell does and exactly what a person
    /// reading the file expects.
    pub fn terminates(&self, line: &str) -> bool {
        let candidate = if self.dash { line.trim_start() } else { line };
        candidate.trim_end().as_bytes() == &self.tag[..self.len as usize]
    }
}

/// What a line was left in the middle of.
///
/// One enum for all six languages rather than one per language, because the
/// cache that stores these stores exactly one type and the editor switches
/// language under it. A variant no current lexer produces for a given language
/// simply never appears in that language's cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Carry {
    /// Nothing: the line closed everything it opened.
    #[default]
    Start,
    /// A quote left open, carried by the quote byte. Shell only — a `"` there
    /// spans lines, and the next line is still inside the string.
    Quote(u8),
    /// A TOML multi-line string, carried by the quote byte it opened with.
    Multiline(u8),
    /// A shell heredoc still waiting for its tag.
    Heredoc(Heredoc),
    /// A YAML block scalar, carrying the indentation of the line that
    /// introduced it: the body is everything indented further than that.
    BlockScalar(u16),
    /// A Dockerfile instruction the previous line ended with a `\`, so this
    /// line continues it rather than starting a new one.
    Continued,
}

/// The state a line ends in, and the state the next one starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineState(pub(crate) Carry);

impl LineState {
    /// The state the first line of a file starts in.
    pub const START: Self = Self(Carry::Start);

    /// Whether nothing is carried over.
    pub const fn is_start(self) -> bool {
        matches!(self.0, Carry::Start)
    }
}

/// A file format the editor knows how to colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// A log, a README, a file nothing here recognised. One run per line, which
    /// is what the editor did for everything before there were lexers.
    #[default]
    Plain,
    /// `sh`, `bash`, `zsh` and the rc files they read.
    Shell,
    /// `.yml`, `.yaml`.
    Yaml,
    /// `.json`.
    Json,
    /// `.toml`.
    Toml,
    /// `.ini`, `.conf`, `.cfg`, `.properties`, `.env`, and the config files
    /// that spell a mapping `key value`.
    Conf,
    /// `Dockerfile`, `Dockerfile.*`, `Containerfile`.
    Dockerfile,
}

impl Language {
    /// The language of a file called `name` whose first line is `first_line`.
    ///
    /// Three rules in order, because each is more certain than the one after
    /// it: the whole name (a `Dockerfile` has no extension to go on, and a
    /// dotfile is *all* extension as far as splitting on `.` is concerned), the
    /// extension, and then — only for a name with no extension at all — the
    /// shebang. The shebang is last because a `.yml` that happens to start with
    /// `#!` is still YAML, and it is consulted at all because half the shell
    /// scripts on a server are called `deploy` rather than `deploy.sh`.
    pub fn detect(name: &str, first_line: &str) -> Self {
        // A caller with a whole path should not be mis-detected on the strength
        // of a directory called `bin.d`.
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let lower = name.to_ascii_lowercase();

        if let Some(language) = Self::by_name(&lower) {
            return language;
        }
        // A leading dot is not an extension: `.bashrc` splits into an empty
        // stem and `bashrc`, which is why the whole-name table above runs
        // first and why this one refuses to look at a name that starts with one.
        if let Some((_, extension)) = lower.rsplit_once('.')
            && !lower.starts_with('.')
        {
            return Self::by_extension(extension);
        }
        Self::by_shebang(first_line)
    }

    /// The language of a name that carries no usable extension.
    fn by_name(lower: &str) -> Option<Self> {
        if lower == "dockerfile"
            || lower == "containerfile"
            || lower.starts_with("dockerfile.")
            || lower.ends_with(".dockerfile")
        {
            return Some(Self::Dockerfile);
        }
        // The rc files a login shell reads. They are shell scripts, whatever
        // else they look like, and they are the files most often opened out of
        // a home directory.
        if matches!(
            lower,
            ".bashrc"
                | ".bash_profile"
                | ".bash_login"
                | ".bash_logout"
                | ".profile"
                | ".zshrc"
                | ".zshenv"
                | ".zprofile"
                | ".zlogin"
                | ".zlogout"
                | ".kshrc"
                | ".shrc"
        ) {
            return Some(Self::Shell);
        }
        // `.env`, and the per-stage files that live beside it.
        if lower == ".env" || lower.starts_with(".env.") {
            return Some(Self::Conf);
        }
        if matches!(
            lower,
            "sshd_config" | "ssh_config" | ".gitconfig" | ".npmrc" | ".editorconfig"
        ) {
            return Some(Self::Conf);
        }
        None
    }

    /// The language an extension names, [`Language::Plain`] when none does.
    fn by_extension(extension: &str) -> Self {
        match extension {
            "sh" | "bash" | "zsh" | "ksh" | "ash" | "mksh" => Self::Shell,
            "yml" | "yaml" => Self::Yaml,
            "json" => Self::Json,
            "toml" => Self::Toml,
            "ini" | "conf" | "cfg" | "properties" | "env" => Self::Conf,
            _ => Self::Plain,
        }
    }

    /// The language a `#!` line names, [`Language::Plain`] when it names none.
    ///
    /// Anything ending in `sh` is treated as a shell — `sh`, `bash`, `zsh`,
    /// `ksh`, `dash`, `fish`. `fish` is not bourne shell and is coloured as if
    /// it were, which costs a handful of keywords and no correctness: the
    /// comments, strings and expansions this highlights are the same in both.
    fn by_shebang(first_line: &str) -> Self {
        let Some(rest) = first_line.strip_prefix("#!") else {
            return Self::Plain;
        };
        let mut words = rest.split_whitespace();
        let Some(mut interpreter) = words.next() else {
            return Self::Plain;
        };
        // `#!/usr/bin/env bash` names the interpreter in the next word.
        if interpreter.rsplit('/').next() == Some("env") {
            let Some(next) = words.next() else {
                return Self::Plain;
            };
            interpreter = next;
        }
        let leaf = interpreter.rsplit('/').next().unwrap_or(interpreter);
        if leaf.ends_with("sh") {
            Self::Shell
        } else {
            Self::Plain
        }
    }

    /// What the comment toggle puts at the head of a line, when the format has
    /// such a thing.
    ///
    /// `#` for everything except JSON, which has no comment syntax at all —
    /// there is nothing to write that a JSON reader would skip, so the toggle
    /// is not offered rather than being offered and producing an invalid file.
    /// [`Language::Plain`] answers `#` and not `None`: a plain buffer is a
    /// config file the detector did not place, far more often than it is prose,
    /// and refusing the toggle there would take away something that worked
    /// before any of this existed.
    pub const fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::Json => None,
            _ => Some("#"),
        }
    }

    /// Whether a line of this language can leave something open for the next
    /// one to finish.
    ///
    /// The cache in [`crate::editor::highlight`] exists only for the languages
    /// that answer `true`. For the others every line is lexed from
    /// [`LineState::START`], so there is nothing worth remembering and a vector
    /// of `START` as long as a hundred-thousand-line log is worth avoiding.
    pub const fn carries_state(self) -> bool {
        match self {
            // A plain line is one run; a JSON string may not contain a newline;
            // an ini or `.env` line is a mapping and ends with itself.
            Self::Plain | Self::Json | Self::Conf => false,
            // Quotes and heredocs; block scalars; multi-line strings; a `\`
            // continuation.
            Self::Shell | Self::Yaml | Self::Toml | Self::Dockerfile => true,
        }
    }
}

/// The tokens of `line`, given the state the line before it ended in.
///
/// The one entry point. Offsets in the tokens are relative to the start of
/// `line`, which must not contain the line break.
pub fn lex_line(line: &str, state: LineState, language: Language) -> (Vec<Token>, LineState) {
    match language {
        Language::Plain => (plain(line), LineState::START),
        Language::Shell => shell::lex_line(line, state),
        Language::Yaml => yaml::lex_line(line, state),
        // The two that carry nothing hand back nothing to carry.
        Language::Json => (json::lex_line(line), LineState::START),
        Language::Toml => toml::lex_line(line, state),
        Language::Conf => (conf::lex_line(line), LineState::START),
        Language::Dockerfile => dockerfile::lex_line(line, state),
    }
}

/// One run over the whole line, which is what the editor drew before it could
/// tell one file from another.
fn plain(line: &str) -> Vec<Token> {
    if line.is_empty() {
        Vec::new()
    } else {
        vec![Token {
            kind: TokenKind::Plain,
            start: 0,
            end: line.len(),
        }]
    }
}

/// Tokens under construction, with the plain runs filled in for free.
///
/// A lexer built on this cannot leave a hole: it says where the interesting
/// spans are, in order, and everything between one and the next becomes a
/// [`TokenKind::Plain`] token when it is passed over. That is the invariant the
/// renderer depends on — the runs it shapes must add up to the line exactly —
/// and it is worth taking out of the hands of six separate loops.
pub(crate) struct Runs {
    /// What has been decided.
    tokens: Vec<Token>,
    /// Where the plain run that is still open begins.
    plain_from: usize,
}

impl Runs {
    /// An empty line's worth.
    pub(crate) const fn new() -> Self {
        Self {
            tokens: Vec::new(),
            plain_from: 0,
        }
    }

    /// Records `at..end` as `kind`, closing any plain run before it.
    ///
    /// `at` must not be behind a span already pushed; a lexer that scans
    /// forwards cannot do otherwise, and one that tried would be ignored rather
    /// than allowed to produce an overlap.
    pub(crate) fn push(&mut self, kind: TokenKind, at: usize, end: usize) {
        let at = at.max(self.plain_from);
        let end = end.max(at);
        if at > self.plain_from {
            self.tokens.push(Token {
                kind: TokenKind::Plain,
                start: self.plain_from,
                end: at,
            });
        }
        if end > at {
            self.tokens.push(Token {
                kind,
                start: at,
                end,
            });
        }
        self.plain_from = end;
    }

    /// The tokens, with the tail of a `len`-byte line closed off.
    pub(crate) fn finish(mut self, len: usize) -> Vec<Token> {
        if len > self.plain_from {
            self.tokens.push(Token {
                kind: TokenKind::Plain,
                start: self.plain_from,
                end: len,
            });
        }
        self.tokens
    }
}

// --- scanning helpers, shared by the six lexers ------------------------------

/// How many bytes the character at `at` takes.
///
/// Read off the lead byte rather than by slicing, so that a caller that has
/// somehow landed off a boundary advances by one byte instead of panicking. A
/// lexer here only ever lands on boundaries — it splits on ASCII and steps by
/// this — but "never panics" is the promise this module is built on.
pub(crate) fn char_step(line: &str, at: usize) -> usize {
    match line.as_bytes().get(at) {
        None => 1,
        Some(0xc0..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf7) => 4,
        Some(_) => 1,
    }
}

/// Whether `at` begins a word rather than continuing one.
pub(crate) fn word_boundary(bytes: &[u8], at: usize) -> bool {
    match at.checked_sub(1).and_then(|before| bytes.get(before)) {
        None => true,
        Some(byte) => !byte.is_ascii_alphanumeric() && *byte != b'_',
    }
}

/// The end of the `[A-Za-z0-9_]` word starting at `at`.
pub(crate) fn word_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    end
}

/// The first byte at or after `at` that is not a space or a tab.
pub(crate) fn skip_spaces(bytes: &[u8], at: usize) -> usize {
    let mut at = at;
    while matches!(bytes.get(at), Some(b' ' | b'\t')) {
        at += 1;
    }
    at
}

/// How many bytes of leading space and tab `line` has.
pub(crate) fn indent_of(line: &str) -> usize {
    skip_spaces(line.as_bytes(), 0)
}

/// The end of a quote body — the byte after the closing `quote` — starting at
/// `at`, which is the first byte *inside* the quote.
///
/// `None` when the line ends before the quote closes, which is the caller's cue
/// to colour the rest of the line and, if its language allows it, carry the
/// quote to the next line.
pub(crate) fn quote_body(line: &str, at: usize, quote: u8, escapes: bool) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut at = at;
    while at < bytes.len() {
        let byte = bytes[at];
        if escapes && byte == b'\\' {
            // A trailing backslash escapes the line break itself, so the string
            // is still open whatever comes next.
            if at + 1 >= bytes.len() {
                return None;
            }
            at += 1 + char_step(line, at + 1);
        } else if byte == quote {
            return Some(at + 1);
        } else {
            at += char_step(line, at);
        }
    }
    None
}

/// The end of a quoted run whose opening quote is at `at`.
pub(crate) fn quoted(line: &str, at: usize, escapes: bool) -> Option<usize> {
    let quote = *line.as_bytes().get(at)?;
    quote_body(line, at + 1, quote, escapes)
}

/// The end of the number starting at `at`.
///
/// Deliberately greedy across `.`, `:` and `-` when a digit follows, so that a
/// version, an IPv4 address and a TOML timestamp each come out as one number
/// rather than as three with punctuation between them. That is a lie about the
/// grammar and the truth about how they read.
pub(crate) fn number(line: &str, at: usize) -> usize {
    let bytes = line.as_bytes();
    let mut end = at;
    if matches!(bytes.get(end), Some(b'-' | b'+')) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'0')
        && matches!(
            bytes.get(end + 1).map(|byte| byte | 32),
            Some(b'x' | b'b' | b'o')
        )
    {
        end += 2;
        while matches!(bytes.get(end), Some(byte) if byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            end += 1;
        }
        return end;
    }
    let digits = |bytes: &[u8], from: usize| {
        let mut at = from;
        while matches!(bytes.get(at), Some(byte) if byte.is_ascii_digit() || *byte == b'_') {
            at += 1;
        }
        at
    };
    end = digits(bytes, end);
    while matches!(bytes.get(end), Some(b'.' | b':' | b'-'))
        && matches!(bytes.get(end + 1), Some(byte) if byte.is_ascii_digit())
    {
        end = digits(bytes, end + 1);
    }
    if matches!(bytes.get(end).map(|byte| byte | 32), Some(b'e')) {
        let mut after = end + 1;
        if matches!(bytes.get(after), Some(b'+' | b'-')) {
            after += 1;
        }
        if matches!(bytes.get(after), Some(byte) if byte.is_ascii_digit()) {
            end = digits(bytes, after);
        }
    }
    end
}

#[cfg(test)]
mod tests;
