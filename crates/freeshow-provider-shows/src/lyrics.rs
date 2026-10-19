use crate::{case_table, limit, unsupported, Budget, Result, SourceValue, MAX_LYRICS, MAX_SLIDES};
#[derive(Clone, Debug, serde::Serialize)]
pub struct Section {
    pub label: String,
    pub lyrics: Vec<String>,
}
pub(crate) fn space(c: char) -> bool {
    matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(space)
}
fn text(s: &str, b: &Budget) -> Result<String> {
    b.work(s.len())?;
    Ok(s.into())
}
fn flush(
    buffer: &mut Vec<&str>,
    section: &mut Section,
    b: &Budget,
    total: &mut usize,
) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    if *total >= MAX_SLIDES {
        return Err(limit());
    }
    let size = buffer.iter().map(|s| s.len()).sum::<usize>() + buffer.len().saturating_sub(1);
    b.work(size + 32)?;
    let joined = buffer.join("\n");
    section.lyrics.push(text(trim(&joined), b)?);
    *total += 1;
    buffer.clear();
    Ok(())
}
pub(crate) fn parse(value: &SourceValue, b: &Budget) -> Result<Vec<Section>> {
    if !value.truthy() {
        return Ok(vec![]);
    }
    let SourceValue::String(s) = value else {
        return Err(unsupported(
            "Truthy lyrics must support source string methods",
        ));
    };
    if s.len() > MAX_LYRICS {
        return Err(limit());
    }
    b.op(s.len())?;
    if !s.starts_with('[') {
        let mut result = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
                let t = trim(&s[start..i]);
                if !t.is_empty() {
                    if result.len() >= MAX_SLIDES {
                        return Err(limit());
                    }
                    result.push(text(t, b)?);
                }
                while bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
                start = i;
            } else {
                i += 1;
            }
        }
        let t = trim(&s[start..]);
        if !t.is_empty() {
            if result.len() >= MAX_SLIDES {
                return Err(limit());
            }
            result.push(text(t, b)?);
        }
        return Ok(vec![Section {
            label: "Lyrics".into(),
            lyrics: result,
        }]);
    }
    let mut sections = Vec::new();
    let mut current: Option<Section> = None;
    let mut buffer = Vec::new();
    let mut line_count = 0;
    let mut total = 0;
    for line in s.split('\n') {
        line_count += 1;
        if line_count > crate::MAX_NODES {
            return Err(limit());
        }
        b.work(16)?;
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(mut section) = current.take() {
                flush(&mut buffer, &mut section, b, &mut total)?;
                if sections.len() >= MAX_SLIDES {
                    return Err(limit());
                }
                sections.push(section);
            }
            let mut label = trim(&line[1..line.len() - 1]);
            let end = label.trim_end_matches(|c: char| c.is_ascii_digit());
            if end.len() != label.len() {
                label = end.trim_end_matches(space);
            }
            current = Some(Section {
                label: text(label, b)?,
                lyrics: Vec::new(),
            });
            buffer.clear();
        } else if trim(line).is_empty() && !buffer.is_empty() {
            if let Some(section) = &mut current {
                flush(&mut buffer, section, b, &mut total)?;
            }
        } else {
            buffer.push(line);
        }
    }
    if let Some(mut section) = current {
        flush(&mut buffer, &mut section, b, &mut total)?;
        if sections.len() >= MAX_SLIDES {
            return Err(limit());
        }
        sections.push(section);
    }
    if sections.iter().map(|s| s.lyrics.len()).sum::<usize>() > MAX_SLIDES {
        return Err(limit());
    }
    Ok(sections)
}
fn in_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let cp = c as u32;
    let i = ranges.partition_point(|(start, _)| *start <= cp);
    i > 0 && cp <= ranges[i - 1].1
}
pub(crate) fn lower(s: &str, b: &Budget) -> Result<String> {
    b.work(s.len() * 8)?;
    let chars = s.chars().collect::<Vec<_>>();
    let mut after = vec![false; chars.len()];
    let mut cased = false;
    for (i, &c) in chars.iter().enumerate().rev() {
        after[i] = cased;
        if !in_ranges(c, case_table::CASE_IGNORABLE) {
            cased = in_ranges(c, case_table::CASED);
        }
    }
    let mut before = false;
    let mut result = String::new();
    for (i, &c) in chars.iter().enumerate() {
        b.op(1)?;
        if c == '\u{3a3}' && before && !after[i] {
            result.push('\u{3c2}');
        } else {
            match case_table::LOWERCASE.binary_search_by_key(&(c as u32), |(cp, _)| *cp) {
                Ok(i) => result.push_str(case_table::LOWERCASE[i].1),
                Err(_) => result.push(c),
            }
        }
        if !in_ranges(c, case_table::CASE_IGNORABLE) {
            before = in_ranges(c, case_table::CASED);
        }
    }
    b.work(result.len())?;
    Ok(result)
}
