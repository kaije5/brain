//! Knowledge document model extraction (format spec §4): title resolution,
//! headings, tags, wikilinks, block IDs, and inline links, with fenced code
//! blocks and inline code spans excluded from inline scanning.

use crate::error::VaultFormatError;
use crate::frontmatter::{FrontmatterBlock, split_frontmatter};

pub(crate) const MAX_TAGS: usize = 64;
const MAX_TAG_BYTES: usize = 64;
const MAX_HEADINGS: usize = 512;
const MAX_HEADING_BYTES: usize = 512;
const MAX_TITLE_BYTES: usize = 256;
const MAX_WIKILINKS: usize = 1024;
const MAX_WIKILINK_TARGET_BYTES: usize = 512;
const MAX_WIKILINK_LABEL_BYTES: usize = 256;
const MAX_BLOCK_IDS: usize = 1024;
const MAX_BLOCK_ID_BYTES: usize = 64;
pub(crate) const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// A parsed `[[target]]` or `[[target|label]]` wikilink.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wikilink {
    pub target: String,
    pub label: Option<String>,
}

/// A parsed inline Markdown reference (`[text](target)`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineLink {
    pub text: String,
    pub target: String,
}

/// The parsed knowledge document model. The body is carried as the original
/// text so unmutated round-trips are byte-for-byte (format spec §7).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedDocument {
    frontmatter: Option<FrontmatterBlock>,
    body: String,
    raw: String,
    title: Option<String>,
    headings: Vec<String>,
    tags: Vec<String>,
    wikilinks: Vec<Wikilink>,
    block_ids: Vec<String>,
    links: Vec<InlineLink>,
}

impl ParsedDocument {
    /// The verbatim frontmatter block, when the document has one.
    #[must_use]
    pub fn frontmatter(&self) -> Option<&FrontmatterBlock> {
        self.frontmatter.as_ref()
    }

    /// The verbatim body (everything after the frontmatter block).
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The verbatim full text. Re-serializing an unmutated document returns
    /// exactly this (format spec §7, promise 5).
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The resolved title: `title` frontmatter, else the first ATX heading,
    /// else the caller-supplied file-stem fallback.
    #[must_use]
    pub fn title(&self, file_stem: &str) -> std::borrow::Cow<'_, str> {
        match self
            .title
            .as_deref()
            .or_else(|| self.headings.first().map(String::as_str))
        {
            Some(title) => std::borrow::Cow::Borrowed(title),
            None => std::borrow::Cow::Owned(file_stem.to_owned()),
        }
    }

    /// ATX headings in body order, markup stripped.
    #[must_use]
    pub fn headings(&self) -> &[String] {
        &self.headings
    }

    /// Deduplicated union of the `tags` frontmatter property and inline
    /// `#tag` occurrences, case-preserving.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Wikilinks in body order.
    #[must_use]
    pub fn wikilinks(&self) -> &[Wikilink] {
        &self.wikilinks
    }

    /// Trailing `^block-id` markers in body order.
    #[must_use]
    pub fn block_ids(&self) -> &[String] {
        &self.block_ids
    }

    /// Inline Markdown references in body order.
    #[must_use]
    pub fn links(&self) -> &[InlineLink] {
        &self.links
    }
}

/// Parses document text into the bounded document model.
///
/// # Errors
/// Returns typed [`VaultFormatError`] values for malformed or oversized
/// input (format spec §8).
pub fn parse_document(raw: &str) -> Result<ParsedDocument, VaultFormatError> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(VaultFormatError::TooLarge);
    }
    let (frontmatter, body) = split_frontmatter(raw)?;
    let mut parsed = ParsedDocument {
        title: None,
        headings: Vec::new(),
        tags: Vec::new(),
        wikilinks: Vec::new(),
        block_ids: Vec::new(),
        links: Vec::new(),
        body: body.to_owned(),
        raw: raw.to_owned(),
        frontmatter,
    };

    // Frontmatter-managed fields. Duplicate managed keys are a typed error
    // on read (unknown keys follow last-wins; format spec §5).
    if let Some(block) = &parsed.frontmatter {
        for managed in ["title", "tags"] {
            if block
                .properties()
                .iter()
                .filter(|property| property.key == managed)
                .count()
                > 1
            {
                return Err(VaultFormatError::DuplicateProperty {
                    field: "frontmatter",
                });
            }
        }
        if let Some(title) = block.scalar("title") {
            if title.len() > MAX_TITLE_BYTES {
                return Err(VaultFormatError::InvalidProperty { field: "title" });
            }
            parsed.title = Some(title.to_owned());
        }
        for tag in block.sequence("tags")? {
            push_tag(&mut parsed.tags, &tag)?;
        }
    }

    scan_body(&mut parsed, body)?;
    Ok(parsed)
}

/// Parses raw file bytes; invalid UTF-8 is a typed error, never repaired.
///
/// # Errors
/// Returns [`VaultFormatError::InvalidEncoding`] for non-UTF-8 input and
/// typed bound errors otherwise.
pub fn parse_document_bytes(bytes: &[u8]) -> Result<ParsedDocument, VaultFormatError> {
    let text = std::str::from_utf8(bytes).map_err(|_| VaultFormatError::InvalidEncoding)?;
    parse_document(text)
}

/// Normalizes CRLF (and lone CR) to LF for a file Brain rewrites anyway
/// (format spec §2).
#[must_use]
pub fn normalize_line_endings(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn scan_body(parsed: &mut ParsedDocument, body: &str) -> Result<(), VaultFormatError> {
    // Fence tracking: an opening fence needs >=3 markers (an info string is
    // allowed); only a marker-only line of the same character closes it.
    let mut fence: Option<char> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some((marker, marker_length)) = leading_marker_run(trimmed) {
            match fence {
                None => fence = Some(marker),
                Some(open) if marker == open && trimmed[marker_length..].trim().is_empty() => {
                    fence = None;
                }
                _ => {}
            }
        }

        if fence.is_none() {
            if let Some(heading) = heading_text(strip_block_id_suffix(trimmed)) {
                if heading.len() > MAX_HEADING_BYTES {
                    return Err(VaultFormatError::TooLarge);
                }
                if parsed.headings.len() >= MAX_HEADINGS {
                    return Err(VaultFormatError::TooLarge);
                }
                if parsed.title.is_none() && trimmed.starts_with("# ") {
                    parsed.title = Some(heading.clone());
                }
                parsed.headings.push(heading);
            }
            scan_inline(parsed, line)?;
            scan_block_id(parsed, trimmed)?;
        }
    }
    Ok(())
}

/// Returns the fence character and its run length when the line starts with
/// at least three identical fence markers.
fn leading_marker_run(trimmed: &str) -> Option<(char, usize)> {
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let length = trimmed
        .chars()
        .take_while(|character| *character == first)
        .count();
    (length >= 3).then_some((first, length))
}

/// Removes a trailing `^block-id` marker (and its separating whitespace)
/// from a line's display text.
fn strip_block_id_suffix(line: &str) -> &str {
    let Some(caret) = line.rfind('^') else {
        return line;
    };
    let candidate = &line[caret + 1..];
    if !candidate.is_empty()
        && candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return line[..caret].trim_end();
    }
    line
}

fn heading_text(trimmed: &str) -> Option<String> {
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    let rest = rest.strip_prefix(' ')?;
    Some(rest.trim_end().to_owned())
}

fn scan_inline(parsed: &mut ParsedDocument, line: &str) -> Result<(), VaultFormatError> {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'`' => {
                // Skip the inline code span entirely.
                if let Some(close) = line[index + 1..].find('`') {
                    index += close + 2;
                } else {
                    index += 1;
                }
            }
            b'[' => {
                if line[index..].starts_with("[[") {
                    if let Some(close) = line[index + 2..].find("]]") {
                        let inner = &line[index + 2..index + 2 + close];
                        push_wikilink(parsed, inner)?;
                        index += close + 4;
                        continue;
                    }
                } else if let Some(close) = line[index + 1..].find(']') {
                    let after = &line[index + close + 2..];
                    if let Some(target) = after.strip_prefix('(')
                        && let Some(target_close) = target.find(')')
                    {
                        let text = &line[index + 1..index + 1 + close];
                        let target = &target[..target_close];
                        if !target.starts_with(':') && !target.contains(' ') {
                            if parsed.links.len() >= MAX_WIKILINKS {
                                return Err(VaultFormatError::TooLarge);
                            }
                            parsed.links.push(InlineLink {
                                text: text.to_owned(),
                                target: target.to_owned(),
                            });
                        }
                        index += close + 2 + target_close + 2;
                        continue;
                    }
                }
                index += 1;
            }
            b'#' => {
                let rest = &line[index..];
                if index > 0 && !bytes[index - 1].is_ascii_whitespace() {
                    index += 1;
                    continue;
                }
                let tag_length = rest[1..]
                    .chars()
                    .take_while(|character| {
                        character.is_alphanumeric() || matches!(character, '/' | '_' | '-' | ':')
                    })
                    .map(char::len_utf8)
                    .sum::<usize>()
                    + 1;
                if tag_length > 1 {
                    if tag_length > MAX_TAG_BYTES {
                        return Err(VaultFormatError::TooLarge);
                    }
                    let tag = rest[..tag_length].trim_start_matches('#').to_owned();
                    push_tag(&mut parsed.tags, &tag)?;
                    index += tag_length;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}

fn push_tag(tags: &mut Vec<String>, tag: &str) -> Result<(), VaultFormatError> {
    let tag = tag.trim();
    if tag.is_empty() || tag.len() > MAX_TAG_BYTES {
        return if tag.is_empty() {
            Ok(())
        } else {
            Err(VaultFormatError::TooLarge)
        };
    }
    if !tags.iter().any(|existing| existing == tag) {
        if tags.len() >= MAX_TAGS {
            return Err(VaultFormatError::TooLarge);
        }
        tags.push(tag.to_owned());
    }
    Ok(())
}

fn push_wikilink(parsed: &mut ParsedDocument, inner: &str) -> Result<(), VaultFormatError> {
    if parsed.wikilinks.len() >= MAX_WIKILINKS {
        return Err(VaultFormatError::TooLarge);
    }
    let (target, label) = match inner.split_once('|') {
        Some((target, label)) => (target.trim(), Some(label.trim())),
        None => (inner.trim(), None),
    };
    if target.is_empty() || target.len() > MAX_WIKILINK_TARGET_BYTES {
        return Err(VaultFormatError::InvalidProperty { field: "wikilink" });
    }
    if let Some(label) = label
        && label.len() > MAX_WIKILINK_LABEL_BYTES
    {
        return Err(VaultFormatError::InvalidProperty { field: "wikilink" });
    }
    parsed.wikilinks.push(Wikilink {
        target: target.to_owned(),
        label: label.filter(|label| !label.is_empty()).map(str::to_owned),
    });
    Ok(())
}

fn scan_block_id(parsed: &mut ParsedDocument, trimmed: &str) -> Result<(), VaultFormatError> {
    // Block IDs are `^id` markers at the end of a line (or alone on a line).
    let Some(caret) = trimmed.rfind('^') else {
        return Ok(());
    };
    let candidate = &trimmed[caret + 1..];
    if candidate.is_empty()
        || !candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Ok(());
    }
    if candidate.len() > MAX_BLOCK_ID_BYTES {
        return Err(VaultFormatError::InvalidProperty { field: "block_id" });
    }
    if parsed.block_ids.len() >= MAX_BLOCK_IDS {
        return Err(VaultFormatError::TooLarge);
    }
    parsed.block_ids.push(candidate.to_owned());
    Ok(())
}
