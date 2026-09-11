//! Frontmatter splitting, scanning, and verbatim preservation (format spec
//! §5). Only the YAML block form is recognized; the raw block text is the
//! preservation unit and is carried through untouched unless a managed
//! property is rewritten.

use std::collections::BTreeSet;

use crate::error::VaultFormatError;

pub(crate) const MAX_FRONTMATTER_BYTES: usize = 8 * 1024;
pub(crate) const MAX_PROPERTY_VALUE_BYTES: usize = 1024;

/// A scanned frontmatter property: the key plus the raw text after `key:`
/// and any block-form (`- item`) sequence items.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawProperty {
    pub key: String,
    pub raw_value: Option<String>,
    pub block_items: Vec<String>,
}

/// The verbatim frontmatter block (without delimiters) plus the scanned
/// property list, in original order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontmatterBlock {
    raw: String,
    properties: Vec<RawProperty>,
}

impl FrontmatterBlock {
    /// The preserved block text, without the `---` delimiters.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Scanned properties in original order.
    #[must_use]
    pub fn properties(&self) -> &[RawProperty] {
        &self.properties
    }

    /// The first scalar value for a key, trimmed of quotes, if present.
    #[must_use]
    pub fn scalar(&self, key: &str) -> Option<&str> {
        let property = self
            .properties
            .iter()
            .find(|property| property.key == key)?;
        let raw = property.raw_value.as_deref()?;
        Some(unescape_scalar(raw))
    }

    /// Sequence items for a key written in block form (`key:` followed by
    /// `- item` lines) or flow form (`key: [a, b]`).
    ///
    /// # Errors
    /// Returns [`VaultFormatError::DuplicateProperty`] when the key appears
    /// more than once, and [`VaultFormatError::InvalidProperty`] when an
    /// item exceeds the value bound.
    pub fn sequence(&self, key: &str) -> Result<Vec<String>, VaultFormatError> {
        let matches: Vec<&RawProperty> = self
            .properties
            .iter()
            .filter(|property| property.key == key)
            .collect();
        if matches.len() > 1 {
            return Err(VaultFormatError::DuplicateProperty {
                field: "frontmatter",
            });
        }
        let mut items = Vec::new();
        for property in matches {
            match property.raw_value.as_deref() {
                Some(value) if value.starts_with('[') => {
                    if !value.ends_with(']') {
                        return Err(VaultFormatError::MalformedFrontmatter);
                    }
                    for item in value[1..value.len() - 1].split(',') {
                        let item = unescape_scalar(item);
                        if !item.is_empty() {
                            check_value_bound(item)?;
                            items.push(item.to_owned());
                        }
                    }
                }
                Some(_) => {
                    // A scalar value where a sequence was expected: treat the
                    // scalar as a single-item sequence, as Obsidian accepts.
                    let item = self.scalar(key).unwrap_or_default();
                    check_value_bound(item)?;
                    items.push(item.to_owned());
                }
                None => {
                    for item in &property.block_items {
                        check_value_bound(item)?;
                        items.push(item.clone());
                    }
                }
            }
        }
        if items.len() > crate::document::MAX_TAGS {
            return Err(VaultFormatError::TooLarge);
        }
        Ok(items)
    }

    /// Rewrites the managed properties (in the canonical §6.1 order) while
    /// preserving every other line of the block byte-for-byte. Existing
    /// lines for managed keys (and their continuations) are replaced by the
    /// canonical emission; unknown properties keep their exact position,
    /// order, and formatting.
    ///
    /// # Errors
    /// Returns [`VaultFormatError::DuplicateProperty`] when a managed key
    /// occurs more than once in the block, which a rewrite cannot resolve
    /// without guessing the user's intent.
    pub fn with_managed_properties(
        &self,
        managed: &[(&str, String)],
    ) -> Result<String, VaultFormatError> {
        let managed_keys: BTreeSet<&str> = managed.iter().map(|(key, _)| *key).collect();
        let mut seen = BTreeSet::new();
        for property in &self.properties {
            if managed_keys.contains(property.key.as_str()) && !seen.insert(property.key.clone()) {
                return Err(VaultFormatError::DuplicateProperty {
                    field: "frontmatter",
                });
            }
        }
        let mut output = String::with_capacity(self.raw.len() + 256);
        let mut emitted_continuations = BTreeSet::new();
        for (key, value) in managed {
            output.push_str(key);
            output.push_str(": ");
            output.push_str(value);
            output.push('\n');
            let _ = emitted_continuations.insert(*key);
        }
        let mut skip_continuations = false;
        for line in self.raw.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                // A continuation belongs to the preceding top-level property;
                // keep it only when that property is not being rewritten.
                if skip_continuations {
                    continue;
                }
                output.push_str(line);
                output.push('\n');
                continue;
            }
            let key = line.split_once(':').map(|(key, _)| key.trim());
            let is_managed_line =
                key.is_some_and(|key| managed_keys.contains(&key)) && line.contains(':');
            skip_continuations = is_managed_line;
            if is_managed_line {
                continue;
            }
            output.push_str(line);
            output.push('\n');
        }
        Ok(output)
    }

    /// Whether any managed key appears in the block at a position that a
    /// rewrite must not silently drop.
    #[must_use]
    pub fn managed_key_conflicts(&self, managed: &[&str]) -> bool {
        self.properties
            .iter()
            .any(|property| managed.contains(&property.key.as_str()))
    }
}

/// Splits raw document text into its optional frontmatter block and the body.
/// The body starts immediately after the closing delimiter line and is
/// returned verbatim.
///
/// # Errors
/// Returns [`VaultFormatError::MalformedFrontmatter`] for an unterminated
/// block and [`VaultFormatError::TooLarge`] when the block exceeds 8 KiB.
pub fn split_frontmatter(raw: &str) -> Result<(Option<FrontmatterBlock>, &str), VaultFormatError> {
    let Some(first_line_end) = raw.find('\n') else {
        return Ok((None, raw));
    };
    let opens = raw.starts_with("---") && {
        let after = &raw[3..first_line_end];
        after == "\r" || after.is_empty()
    };
    if !opens {
        return Ok((None, raw));
    }
    let content_start = first_line_end + 1;
    let mut cursor = content_start;
    let mut close_line_start = None;
    for line in raw[content_start..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            close_line_start = Some(cursor);
            break;
        }
        cursor += line.len();
    }
    let Some(close_start) = close_line_start else {
        return Err(VaultFormatError::MalformedFrontmatter);
    };
    let block_text = &raw[content_start..close_start];
    if block_text.len() > MAX_FRONTMATTER_BYTES {
        return Err(VaultFormatError::TooLarge);
    }
    let close_line_end = raw[close_start..]
        .find('\n')
        .map_or(raw.len(), |position| close_start + position + 1);
    let properties = scan_properties(block_text)?;
    Ok((
        Some(FrontmatterBlock {
            raw: block_text.to_owned(),
            properties,
        }),
        &raw[close_line_end..],
    ))
}

fn scan_properties(block: &str) -> Result<Vec<RawProperty>, VaultFormatError> {
    let mut properties: Vec<RawProperty> = Vec::new();
    for line in block.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // A sequence item or continuation for the most recent property.
            let trimmed = line.trim();
            if let Some(item) = trimmed.strip_prefix("- ") {
                check_value_bound(item)?;
                if let Some(last) = properties.last_mut() {
                    last.block_items.push(item.to_owned());
                }
            }
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            return Err(VaultFormatError::MalformedFrontmatter);
        };
        let key = key.trim();
        if key.is_empty() || key.len() > 128 {
            return Err(VaultFormatError::MalformedFrontmatter);
        }
        let raw_value = rest.trim();
        if !raw_value.is_empty() {
            check_value_bound(raw_value)?;
        }
        properties.push(RawProperty {
            key: key.to_owned(),
            raw_value: (!raw_value.is_empty()).then(|| raw_value.to_owned()),
            block_items: Vec::new(),
        });
    }
    Ok(properties)
}

fn check_value_bound(value: &str) -> Result<(), VaultFormatError> {
    if value.len() > MAX_PROPERTY_VALUE_BYTES {
        return Err(VaultFormatError::TooLarge);
    }
    Ok(())
}

/// Strips one layer of matching quotes and surrounding whitespace.
fn unescape_scalar(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return &trimmed[1..trimmed.len() - 1];
    }
    trimmed
}
